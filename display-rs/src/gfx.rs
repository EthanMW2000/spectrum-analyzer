//! A small GPU 2D batcher on wgpu: coloured/gradient quads, rounded rects,
//! circles, images and text from a glyph atlas. The phosphor background
//! (glow + grain) and the vignette + scanlines are fragment shaders, so no
//! full-screen alpha layers are composited on the CPU.

use std::collections::HashMap;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use winit::window::Window;

pub type Rgba = [f32; 4];

pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Rgba {
    [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ]
}

const MODE_SOLID: f32 = 0.0;
const MODE_IMAGE: f32 = 1.0;
const MODE_GLYPH: f32 = 2.0;
const MODE_RRECT: f32 = 3.0;
const MODE_RTOP: f32 = 4.0;
const MODE_CIRCLE: f32 = 5.0;
const MODE_OUTLINE: f32 = 6.0;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
    local: [f32; 2],
    size: [f32; 2],
    params: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    screen: [f32; 2],
    pad: [f32; 2],
}

const SHADER: &str = r#"
struct Globals { screen: vec2<f32>, pad: vec2<f32> };
@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(1) @binding(0) var img: texture_2d<f32>;

struct VIn {
  @location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>,
  @location(3) local: vec2<f32>, @location(4) size: vec2<f32>, @location(5) params: vec2<f32>,
};
struct VOut {
  @builtin(position) clip: vec4<f32>, @location(0) uv: vec2<f32>, @location(1) color: vec4<f32>,
  @location(2) local: vec2<f32>, @location(3) size: vec2<f32>, @location(4) params: vec2<f32>,
};

@vertex fn vs(v: VIn) -> VOut {
  var o: VOut;
  o.clip = vec4<f32>(v.pos.x / g.screen.x * 2.0 - 1.0, 1.0 - v.pos.y / g.screen.y * 2.0, 0.0, 1.0);
  o.uv = v.uv; o.color = v.color; o.local = v.local; o.size = v.size; o.params = v.params;
  return o;
}

fn rrect_sd(p: vec2<f32>, size: vec2<f32>, r: f32) -> f32 {
  let c = size * 0.5;
  let q = abs(p - c) - (c - vec2<f32>(r, r));
  return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
  let mode = i32(i.params.y + 0.5);
  let ti = textureSample(img, samp, i.uv);
  let ta = textureSample(atlas, samp, i.uv).r;
  var c = i.color;
  if (mode == 1) {
    c = c * ti;
  } else if (mode == 2) {
    c.a = c.a * ta;
  } else if (mode == 3) {
    c.a = c.a * clamp(0.5 - rrect_sd(i.local, i.size, i.params.x), 0.0, 1.0);
  } else if (mode == 4) {
    let s = vec2<f32>(i.size.x, i.size.y + i.params.x);
    c.a = c.a * clamp(0.5 - rrect_sd(i.local, s, i.params.x), 0.0, 1.0);
  } else if (mode == 5) {
    let r = i.size.x * 0.5;
    c.a = c.a * clamp(r - length(i.local - vec2<f32>(r, r)) + 0.5, 0.0, 1.0);
  } else if (mode == 6) {
    let d = rrect_sd(i.local, i.size, i.params.x);
    c.a = c.a * clamp(1.0 - abs(d + 0.5), 0.0, 1.0);
  }
  return c;
}

@vertex fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
  let x = f32((i << 1u) & 2u) * 2.0 - 1.0;
  let y = f32(i & 2u) * 2.0 - 1.0;
  return vec4<f32>(x, y, 0.0, 1.0);
}

fn ihash(x: u32, y: u32) -> u32 {
  var h = x * 374761393u + y * 668265263u;
  h = (h ^ (h >> 13u)) * 1274126177u;
  return h ^ (h >> 16u);
}

// COLOR_BG + make_background_glow() + make_grain() from hifi_display.py
@fragment fn fs_bg(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
  let bg = vec3<f32>(14.0, 18.0, 15.0) / 255.0;
  let c = g.screen * 0.5;
  let d = length(floor(p.xy) - c) / length(c);
  let f = exp(-d * d * 3.5);
  let glow = vec3<f32>(0.0, floor(f * 80.0), floor(f * 35.0)) / 255.0;
  var col = mix(bg, glow, floor(f * 95.0) / 255.0);
  let n = f32(ihash(u32(p.x), u32(p.y)) % 14u);
  let grain = vec3<f32>(floor(n / 8.0), floor(n / 2.0), 0.0) / 255.0;
  col = mix(col, grain, 28.0 / 255.0);
  return vec4<f32>(col, 1.0);
}

// make_vignette() + make_scanlines()
@fragment fn fs_overlay(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
  let c = g.screen * 0.5;
  let maxr = length(c);
  let r = ceil(length(floor(p.xy) - c) / 6.0) * 6.0;
  let va = floor(50.0 * pow(min(r / maxr, 1.0), 2.2)) / 255.0;
  let sa = select(0.0, 12.0 / 255.0, (u32(p.y) % 2u) == 0u);
  return vec4<f32>(0.0, 0.0, 0.0, 1.0 - (1.0 - va) * (1.0 - sa));
}
"#;

#[derive(Clone, Copy)]
struct Glyph {
    uv: [f32; 4],
    w: f32,
    h: f32,
    xmin: f32,
    ymin: f32,
    advance: f32,
}

const ATLAS: u32 = 2048;
// On the Pi 5 (V3DV) glyphs in the leftmost ~100 atlas columns come out blank, so leave them unused.
const ATLAS_MARGIN: u32 = 128;

struct Image {
    _tex: wgpu::Texture,
    bind: wgpu::BindGroup,
}

pub struct Gfx {
    pub window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bg_pipeline: wgpu::RenderPipeline,
    overlay_pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    bind0: wgpu::BindGroup,
    img_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    images: Vec<Option<Image>>,
    atlas: wgpu::Texture,
    atlas_cpu: Vec<u8>,
    atlas_dirty: Option<(u32, u32)>,
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    verts: Vec<Vertex>,
    idx: Vec<u32>,
    draws: Vec<(usize, u32, u32)>,
    cur_img: usize,
    font: fontdue::Font,
    glyphs: HashMap<(char, u32), Option<Glyph>>,
    shelf: (u32, u32, u32),
    pub width: f32,
    pub height: f32,
}

impl Gfx {
    pub fn new(window: Arc<Window>, font_bytes: Vec<u8>) -> Gfx {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN | wgpu::Backends::GL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance.create_surface(window.clone()).expect("surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .expect("no GPU adapter");
        let info = adapter.get_info();
        eprintln!("gpu: {} ({:?}, {})", info.name, info.backend, info.driver);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits:
                wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .expect("device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .unwrap();
        config.format = format;
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 2;
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas"),
            size: wgpu::Extent3d {
                width: ATLAS,
                height: ATLAS,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let img_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[tex_entry(0)],
        });
        let bind0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout0,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: globals.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &atlas.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout0), Some(&img_layout)],
            immediate_size: 0,
        });
        let blend = Some(wgpu::ColorTargetState {
            format,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        });
        let attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32x2, 4 => Float32x2, 5 => Float32x2];
        let make = |vs: &str, fs: &str, buffers: &[Option<wgpu::VertexBufferLayout>]| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: None,
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers,
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: std::slice::from_ref(&blend),
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let vlayout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attrs,
        };
        let pipeline = make("vs", "fs", &[Some(vlayout)]);
        let bg_pipeline = make("vs_full", "fs_bg", &[]);
        let overlay_pipeline = make("vs_full", "fs_overlay", &[]);
        let vbuf = Self::buffer(&device, wgpu::BufferUsages::VERTEX, 1 << 20);
        let ibuf = Self::buffer(&device, wgpu::BufferUsages::INDEX, 1 << 19);
        let font =
            fontdue::Font::from_bytes(font_bytes, fontdue::FontSettings::default()).expect("font");
        let mut gfx = Gfx {
            window,
            surface,
            device,
            queue,
            config,
            pipeline,
            bg_pipeline,
            overlay_pipeline,
            globals,
            bind0,
            img_layout,
            sampler,
            images: Vec::new(),
            atlas,
            atlas_cpu: vec![0; (ATLAS * ATLAS * 4) as usize],
            atlas_dirty: Some((0, ATLAS)),
            vbuf,
            ibuf,
            verts: Vec::new(),
            idx: Vec::new(),
            draws: Vec::new(),
            cur_img: 0,
            font,
            glyphs: HashMap::new(),
            shelf: (ATLAS_MARGIN, 0, 0),
            width: size.width.max(1) as f32,
            height: size.height.max(1) as f32,
        };
        let white = image::RgbaImage::from_pixel(1, 1, image::Rgba([255; 4]));
        gfx.set_image(0, &white);
        gfx
    }

    fn buffer(device: &wgpu::Device, usage: wgpu::BufferUsages, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
        self.width = w as f32;
        self.height = h as f32;
    }

    /// Uploads an RGBA image into slot `id` (slot 0 is plain white).
    pub fn set_image(&mut self, id: usize, img: &image::RgbaImage) {
        let (w, h) = img.dimensions();
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            img.as_raw(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.img_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&tex.create_view(&Default::default())),
            }],
        });
        let _ = &self.sampler;
        if self.images.len() <= id {
            self.images.resize_with(id + 1, || None);
        }
        self.images[id] = Some(Image { _tex: tex, bind });
    }

    fn use_image(&mut self, id: usize) {
        if id != self.cur_img {
            self.flush_draw();
            self.cur_img = id;
        }
    }

    fn flush_draw(&mut self) {
        let start = self.draws.last().map(|d| d.1 + d.2).unwrap_or(0);
        let count = self.idx.len() as u32 - start;
        if count > 0 {
            self.draws.push((self.cur_img, start, count));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn quad(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        colors: [Rgba; 4],
        uv: [f32; 4],
        mode: f32,
        radius: f32,
    ) {
        let base = self.verts.len() as u32;
        let corners = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        for (k, (cx, cy)) in corners.iter().enumerate() {
            self.verts.push(Vertex {
                pos: [x + cx * w, y + cy * h],
                uv: [uv[0] + cx * (uv[2] - uv[0]), uv[1] + cy * (uv[3] - uv[1])],
                color: colors[k],
                local: [cx * w, cy * h],
                size: [w, h],
                params: [radius, mode],
            });
        }
        self.idx
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: Rgba) {
        self.quad(x, y, w, h, [c; 4], [0.0; 4], MODE_SOLID, 0.0);
    }

    pub fn rect_hgrad(&mut self, x: f32, y: f32, w: f32, h: f32, l: Rgba, r: Rgba) {
        self.quad(x, y, w, h, [l, r, r, l], [0.0; 4], MODE_SOLID, 0.0);
    }

    /// 1 px rectangle outline (pygame.draw.rect(..., 1)).
    pub fn rect_outline(&mut self, x: f32, y: f32, w: f32, h: f32, c: Rgba) {
        self.rect(x, y, w, 1.0, c);
        self.rect(x, y + h - 1.0, w, 1.0, c);
        self.rect(x, y + 1.0, 1.0, h - 2.0, c);
        self.rect(x + w - 1.0, y + 1.0, 1.0, h - 2.0, c);
    }

    pub fn rrect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgba) {
        self.quad(x, y, w, h, [c; 4], [0.0; 4], MODE_RRECT, r);
    }

    pub fn rrect_outline(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgba) {
        self.quad(x, y, w, h, [c; 4], [0.0; 4], MODE_OUTLINE, r);
    }

    /// Vertical gradient with only the top corners rounded (spectrum bars).
    #[allow(clippy::too_many_arguments)]
    pub fn bar(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, top: Rgba, bottom: Rgba) {
        let mode = if r > 1.0 { MODE_RTOP } else { MODE_SOLID };
        self.quad(x, y, w, h, [top, top, bottom, bottom], [0.0; 4], mode, r);
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, c: Rgba) {
        self.quad(
            cx - r,
            cy - r,
            2.0 * r,
            2.0 * r,
            [c; 4],
            [0.0; 4],
            MODE_CIRCLE,
            0.0,
        );
    }

    pub fn image(&mut self, id: usize, x: f32, y: f32, w: f32, h: f32) {
        if self.images.get(id).is_none_or(|i| i.is_none()) {
            return;
        }
        self.use_image(id);
        self.quad(
            x,
            y,
            w,
            h,
            [[1.0; 4]; 4],
            [0.0, 0.0, 1.0, 1.0],
            MODE_IMAGE,
            0.0,
        );
        self.use_image(0);
    }

    fn glyph(&mut self, ch: char, px: u32) -> Option<Glyph> {
        if let Some(g) = self.glyphs.get(&(ch, px)) {
            return *g;
        }
        let (m, bitmap) = self.font.rasterize(ch, px as f32);
        let (w, h) = (m.width as u32, m.height as u32);
        let mut uv = [0.0; 4];
        if w > 0 && h > 0 {
            let (mut sx, mut sy, mut sh) = self.shelf;
            sx = sx.max(ATLAS_MARGIN);
            if sx + w + 1 > ATLAS {
                sx = ATLAS_MARGIN;
                sy += sh + 1;
                sh = 0;
            }
            if sy + h + 1 > ATLAS {
                // Atlas full: start over (only happens with many distinct sizes).
                self.glyphs.clear();
                self.atlas_cpu.fill(0);
                self.atlas_dirty = Some((0, ATLAS));
                sx = ATLAS_MARGIN;
                sy = 0;
                sh = 0;
            }
            for row in 0..h {
                for col in 0..w {
                    let dst = (((sy + row) * ATLAS + sx + col) * 4) as usize;
                    let v = bitmap[(row * w + col) as usize];
                    self.atlas_cpu[dst..dst + 4].copy_from_slice(&[v; 4]);
                }
            }
            let (d0, d1) = self.atlas_dirty.unwrap_or((sy, sy + h));
            self.atlas_dirty = Some((d0.min(sy), d1.max(sy + h)));
            let a = ATLAS as f32;
            uv = [
                sx as f32 / a,
                sy as f32 / a,
                (sx + w) as f32 / a,
                (sy + h) as f32 / a,
            ];
            self.shelf = (sx + w + 1, sy, sh.max(h));
        }
        let g = Some(Glyph {
            uv,
            w: w as f32,
            h: h as f32,
            xmin: m.xmin as f32,
            ymin: m.ymin as f32,
            advance: m.advance_width,
        });
        self.glyphs.insert((ch, px), g);
        g
    }

    fn line_metrics(&self, px: u32) -> (f32, f32) {
        let lm = self.font.horizontal_line_metrics(px as f32).unwrap();
        (lm.ascent.ceil(), (lm.ascent - lm.descent).ceil())
    }

    /// Width and height of `s` like pygame's Font.render().get_size().
    pub fn measure(&mut self, s: &str, px: u32) -> (f32, f32) {
        let (_, height) = self.line_metrics(px);
        let mut w = 0.0;
        let mut prev = None;
        for ch in s.chars() {
            if let Some(p) = prev {
                w += self.font.horizontal_kern(p, ch, px as f32).unwrap_or(0.0);
            }
            if let Some(g) = self.glyph(ch, px) {
                w += g.advance;
            }
            prev = Some(ch);
        }
        (w.ceil(), height)
    }

    /// Draws `s` with its box's top-left corner at (x, y); returns the box size.
    pub fn text(&mut self, s: &str, px: u32, x: f32, y: f32, c: Rgba) -> (f32, f32) {
        let (ascent, height) = self.line_metrics(px);
        let baseline = (y + ascent).round();
        let mut pen = x.round();
        let mut prev = None;
        for ch in s.chars() {
            if let Some(p) = prev {
                pen += self.font.horizontal_kern(p, ch, px as f32).unwrap_or(0.0);
            }
            if let Some(g) = self.glyph(ch, px) {
                if g.w > 0.0 {
                    let gx = (pen + g.xmin).round();
                    let gy = baseline - g.ymin - g.h;
                    self.quad(gx, gy, g.w, g.h, [c; 4], g.uv, MODE_GLYPH, 0.0);
                }
                pen += g.advance;
            }
            prev = Some(ch);
        }
        ((pen - x).ceil(), height)
    }

    pub fn begin(&mut self) {
        self.verts.clear();
        self.idx.clear();
        self.draws.clear();
        self.cur_img = 0;
    }

    /// Uploads the glyph rows added this frame (whole rows, so copies stay aligned).
    fn upload_atlas(&mut self) {
        let Some((y0, y1)) = self.atlas_dirty.take() else {
            return;
        };
        let start = (y0 * ATLAS * 4) as usize;
        let end = (y1 * ATLAS * 4) as usize;
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.atlas,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: y0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &self.atlas_cpu[start..end],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ATLAS * 4),
                rows_per_image: Some(y1 - y0),
            },
            wgpu::Extent3d {
                width: ATLAS,
                height: y1 - y0,
                depth_or_array_layers: 1,
            },
        );
    }

    pub fn end(&mut self) {
        self.flush_draw();
        self.upload_atlas();
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            _ => return,
        };
        let view = frame.texture.create_view(&Default::default());
        self.queue.write_buffer(
            &self.globals,
            0,
            bytemuck::bytes_of(&Globals {
                screen: [self.width, self.height],
                pad: [0.0; 2],
            }),
        );
        let vbytes: &[u8] = bytemuck::cast_slice(&self.verts);
        let ibytes: &[u8] = bytemuck::cast_slice(&self.idx);
        if vbytes.len() as u64 > self.vbuf.size() {
            self.vbuf = Self::buffer(
                &self.device,
                wgpu::BufferUsages::VERTEX,
                vbytes.len().next_power_of_two() as u64,
            );
        }
        if ibytes.len() as u64 > self.ibuf.size() {
            self.ibuf = Self::buffer(
                &self.device,
                wgpu::BufferUsages::INDEX,
                ibytes.len().next_power_of_two() as u64,
            );
        }
        if !vbytes.is_empty() {
            self.queue.write_buffer(&self.vbuf, 0, vbytes);
            self.queue.write_buffer(&self.ibuf, 0, ibytes);
        }
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let white = &self.images[0].as_ref().unwrap().bind;
            pass.set_bind_group(0, &self.bind0, &[]);
            pass.set_bind_group(1, white, &[]);
            pass.set_pipeline(&self.bg_pipeline);
            pass.draw(0..3, 0..1);
            if !self.idx.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.vbuf.slice(..));
                pass.set_index_buffer(self.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                for &(img, start, count) in &self.draws {
                    let bind = self
                        .images
                        .get(img)
                        .and_then(|i| i.as_ref())
                        .map(|i| &i.bind)
                        .unwrap_or(white);
                    pass.set_bind_group(1, bind, &[]);
                    pass.draw_indexed(start..start + count, 0, 0..1);
                }
            }
            pass.set_bind_group(1, white, &[]);
            pass.set_pipeline(&self.overlay_pipeline);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);
        self.window.pre_present_notify();
        self.queue.present(frame);
    }
}
