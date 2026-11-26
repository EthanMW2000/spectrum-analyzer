//! Screen layout and drawing, ported from the Renderer class in hifi_display.py.

use std::sync::{Arc, Mutex};

use chrono::Local;
use rand::RngExt;

use crate::audio::NUM_BARS;
use crate::gfx::{Gfx, Rgba, rgba};
use crate::ident::{Record, load_art};

pub const IMG_ART: usize = 1;
pub const IMG_PICK: usize = 2;

const COLOR_BAR_LOW: [f32; 3] = [0.0, 230.0, 95.0];
const COLOR_BAR_MID: [f32; 3] = [0.0, 200.0, 130.0];
const COLOR_BAR_HIGH: [f32; 3] = [200.0, 245.0, 200.0];

pub struct Layout {
    pub w: f32,
    pub h: f32,
    title_px: u32,
    artist_px: u32,
    small_px: u32,
    clock_px: u32,
    date_px: u32,
    btn_px: u32,
    info_h: f32,
    spec_h: f32,
    pub art_size: f32,
    art_x: f32,
    art_y: f32,
    text_x: f32,
    bar_gap: f32,
    bar_w: f32,
    bar_x0: f32,
    pub pick_art_size: f32,
}

impl Layout {
    pub fn new(w: f32, h: f32) -> Layout {
        let (wi, hi) = (w as i32, h as i32);
        let info_h = (h * 0.34).floor();
        let art_size = (info_h * 0.78).floor();
        let art_x = (w * 0.025).floor();
        let art_y = ((info_h - art_size) / 2.0).floor();
        let bar_gap = (wi / 320).max(2) as f32;
        let bar_w = ((w - (NUM_BARS as f32 - 1.0) * bar_gap) / NUM_BARS as f32).floor();
        let bar_x0 =
            ((w - (bar_w * NUM_BARS as f32 + bar_gap * (NUM_BARS as f32 - 1.0))) / 2.0).floor();
        Layout {
            w,
            h,
            title_px: (hi / 18).max(20) as u32,
            artist_px: (hi / 24).max(16) as u32,
            small_px: (hi / 38).max(11) as u32,
            clock_px: (h * 0.28) as u32,
            date_px: (h * 0.06) as u32,
            btn_px: (h * 0.05) as u32,
            info_h,
            spec_h: h - info_h - 5.0,
            art_size,
            art_x,
            art_y,
            text_x: art_x + art_size + (w * 0.03).floor(),
            bar_gap,
            bar_w,
            bar_x0,
            pick_art_size: (h * 0.28).floor(),
        }
    }
}

struct Particle {
    x: f32,
    y: f32,
    size: f32,
    vx: f32,
    vy: f32,
    alpha: f32,
    flicker_speed: f32,
    flicker_offset: f32,
    draw_alpha: f32,
    life: u32,
    max_life: u32,
}

impl Particle {
    fn new(l: &Layout, initial: bool) -> Particle {
        let mut r = rand::rng();
        let speed = r.random_range(0.08..0.32);
        Particle {
            x: r.random_range(0.0..l.w),
            y: if initial {
                r.random_range(0.0..l.h)
            } else {
                l.h + 10.0
            },
            size: r.random_range(0.8..2.2),
            vx: r.random_range(-0.1..0.1),
            vy: -speed,
            alpha: r.random_range(25..=120) as f32,
            flicker_speed: r.random_range(0.01..0.04),
            flicker_offset: r.random_range(0.0..std::f32::consts::TAU),
            draw_alpha: 0.0,
            life: 0,
            max_life: r.random_range(400..=1200),
        }
    }

    fn update(&mut self, frame: f32, l: &Layout) {
        self.x += self.vx + (frame * 0.01 + self.flicker_offset).sin() * 0.12;
        self.y += self.vy;
        self.life += 1;
        let flicker = (frame * self.flicker_speed + self.flicker_offset).sin();
        self.draw_alpha = (self.alpha + flicker * 20.0).clamp(0.0, 160.0).floor();
        let ml = self.max_life as f32;
        if self.life as f32 > ml * 0.8 {
            let fade = 1.0 - (self.life as f32 - ml * 0.8) / (ml * 0.2);
            self.draw_alpha = (self.draw_alpha * fade).floor();
        }
        if self.life >= self.max_life || self.y < -20.0 {
            *self = Particle::new(l, false);
        }
    }
}

#[derive(Default)]
pub struct Pick {
    pub record: Option<Record>,
    pub art: Option<image::RgbaImage>,
    pub generation: u64,
}

pub struct SongView<'a> {
    pub title: Option<&'a str>,
    pub artist: Option<&'a str>,
    pub has_art: bool,
}

pub struct Ui {
    pub l: Layout,
    particles: Vec<Particle>,
    frame: u64,
    pub pick: Arc<Mutex<Pick>>,
    pub btn: Option<(f32, f32, f32, f32)>,
    pub pick_shown: bool,
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [0, 1, 2].map(|i| (a[i] + (b[i] - a[i]) * t).floor())
}

fn bar_color(v: f32) -> [f32; 3] {
    if v < 0.5 {
        lerp3(COLOR_BAR_LOW, COLOR_BAR_MID, v / 0.5)
    } else {
        lerp3(COLOR_BAR_MID, COLOR_BAR_HIGH, (v - 0.5) / 0.5)
    }
}

fn c3(c: [f32; 3], scale: f32) -> Rgba {
    [
        (c[0] * scale).floor() / 255.0,
        (c[1] * scale).floor() / 255.0,
        (c[2] * scale).floor() / 255.0,
        1.0,
    ]
}

fn with_alpha(mut c: Rgba, a: f32) -> Rgba {
    c[3] = a.clamp(0.0, 255.0) / 255.0;
    c
}

impl Ui {
    pub fn new(w: f32, h: f32) -> Ui {
        let l = Layout::new(w, h);
        let particles = (0..100).map(|_| Particle::new(&l, true)).collect();
        Ui {
            l,
            particles,
            frame: 0,
            pick: Arc::new(Mutex::new(Pick::default())),
            btn: None,
            pick_shown: false,
        }
    }

    pub fn resize(&mut self, w: f32, h: f32) {
        if (w, h) != (self.l.w, self.l.h) {
            self.l = Layout::new(w, h);
        }
    }

    pub fn spin(&self, records: Vec<Record>) {
        if records.is_empty() {
            return;
        }
        let rec = records[rand::rng().random_range(0..records.len())].clone();
        let generation = {
            let mut p = self.pick.lock().unwrap();
            p.record = Some(rec.clone());
            p.art = None;
            p.generation += 1;
            p.generation
        };
        let pick = self.pick.clone();
        let px = self.l.pick_art_size as u32;
        std::thread::spawn(move || {
            let art = rec.cover_url.as_deref().and_then(|u| load_art(u, px));
            let mut p = pick.lock().unwrap();
            if p.generation == generation {
                p.art = art;
                p.generation += 1;
            }
        });
    }

    pub fn clear_pick(&self) {
        let mut p = self.pick.lock().unwrap();
        if p.record.is_some() {
            p.record = None;
            p.art = None;
            p.generation += 1;
        }
    }

    pub fn draw(&mut self, g: &mut Gfx, bars: &[f32; NUM_BARS], song: &SongView, idle: bool) {
        if idle {
            self.draw_idle(g);
        } else {
            self.draw_info(g, song);
            g.rect(0.0, self.l.info_h, self.l.w, 1.0, rgba(45, 60, 45, 255));
            self.draw_spectrum(g, bars);
        }
        self.frame += 1;
    }

    fn draw_idle(&mut self, g: &mut Gfx) {
        let l = &self.l;
        let frame = self.frame as f32;
        for p in self.particles.iter_mut() {
            p.update(frame, l);
            let r = (p.size as i32).max(1) as f32;
            let (x, y) = (p.x.floor() + 0.5, p.y.floor() + 0.5);
            g.circle(x, y, r + 1.5, rgba(0, 200, 80, (p.draw_alpha as u8) / 5));
            g.circle(x, y, r + 0.5, rgba(0, 200, 80, p.draw_alpha as u8));
        }
        let cx = (l.w / 2.0).floor();
        let now = Local::now();
        let colon = if now.format("%S").to_string().parse::<u32>().unwrap_or(0) % 2 == 0 {
            ":"
        } else {
            " "
        };
        let time_str = now.format(&format!("%H{colon}%M")).to_string();
        let (tw, th) = g.measure(&time_str, l.clock_px);
        let time_x = cx - (tw / 2.0).floor();
        let time_y = (l.h / 2.0).floor() - (th / 2.0).floor() - (l.h * 0.05).floor();
        for (o, a) in [(3.0, 12), (2.0, 22), (1.0, 38)] {
            g.text(
                &time_str,
                l.clock_px,
                time_x + o,
                time_y + o,
                rgba(0, 90, 35, a),
            );
        }
        g.text(&time_str, l.clock_px, time_x, time_y, rgba(0, 220, 90, 255));
        let sec = now.format(":%S").to_string();
        let (_, sh) = g.measure(&sec, l.date_px);
        g.text(
            &sec,
            l.date_px,
            time_x + tw + (l.w * 0.005).floor(),
            time_y + th - sh - (l.h * 0.01).floor(),
            rgba(0, 130, 55, 255),
        );
        let date = now.format("%A,  %B %-d  %Y").to_string();
        let (dw, _) = g.measure(&date, l.date_px);
        g.text(
            &date,
            l.date_px,
            cx - (dw / 2.0).floor(),
            time_y + th + (l.h * 0.02).floor(),
            rgba(60, 110, 70, 255),
        );

        let pick = self.pick.lock().unwrap();
        let has_pick = pick.record.is_some();
        if let Some(rec) = &pick.record {
            let s = l.pick_art_size;
            let ax = (l.w * 0.06).floor();
            let ay = l.h - s - (l.h * 0.06).floor();
            g.rect(ax, ay, s, s, rgba(10, 14, 11, 255));
            if pick.art.is_some() && self.pick_shown {
                g.image(IMG_PICK, ax, ay, s, s);
            }
            for inset in 0..2 {
                let i = inset as f32;
                g.rect_outline(
                    ax + i,
                    ay + i,
                    s - 2.0 * i,
                    s - 2.0 * i,
                    rgba(0, 80, 40, 255),
                );
            }
            let tx = ax + s + (l.w * 0.03).floor();
            let ty = ay + (s * 0.15).floor();
            let (_, th2) = g.text(&rec.title, l.title_px, tx, ty, rgba(230, 240, 230, 255));
            g.text(
                &rec.artist,
                l.artist_px,
                tx,
                ty + th2 + 4.0,
                rgba(155, 180, 155, 255),
            );
        }
        drop(pick);
        let label = if has_pick { "SPIN AGAIN" } else { "SPIN" };
        let (bw0, bh0) = g.measure(label, l.btn_px);
        let bw = bw0 + (l.w * 0.06).floor();
        let bh = bh0 + (l.h * 0.03).floor();
        let bx = cx - (bw / 2.0).floor();
        let by = if has_pick {
            (l.h * 0.04).floor()
        } else {
            l.h - bh - (l.h * 0.04).floor()
        };
        self.btn = Some((bx, by, bw, bh));
        g.rrect(bx, by, bw, bh, 6.0, rgba(0, 60, 30, 255));
        g.rrect_outline(bx, by, bw, bh, 6.0, rgba(0, 140, 60, 255));
        g.text(
            label,
            l.btn_px,
            bx + ((bw - bw0) / 2.0).floor(),
            by + ((bh - bh0) / 2.0).floor(),
            rgba(0, 200, 80, 255),
        );
    }

    fn draw_info(&mut self, g: &mut Gfx, s: &SongView) {
        let l = &self.l;
        let frame = self.frame as f32;
        g.rect(
            l.art_x,
            l.art_y,
            l.art_size,
            l.art_size,
            rgba(10, 14, 11, 255),
        );
        if s.has_art {
            g.image(IMG_ART, l.art_x, l.art_y, l.art_size, l.art_size);
        } else {
            let cx = l.art_x + (l.art_size / 2.0).floor();
            let cy = l.art_y + (l.art_size / 2.0).floor();
            let n = 16;
            let bw = ((l.art_size as i32) / (n * 2)).max(2) as f32;
            let start = cx - ((n as f32 * bw * 2.0) / 2.0).floor();
            let t = frame * 0.05;
            for i in 0..n {
                let fi = i as f32;
                let h = (l.art_size
                    * 0.12
                    * (0.3 + 0.7 * (t + fi * 0.5).sin().abs())
                    * (0.5 + 0.5 * (t * 0.7 + fi * 0.3).sin()))
                .floor()
                .max(2.0);
                g.rect(
                    start + fi * bw * 2.0,
                    cy - (h / 2.0).floor(),
                    bw,
                    h,
                    rgba(0, 120, 55, 255),
                );
            }
        }
        for inset in 0..3 {
            let i = inset as f32;
            g.rect_outline(
                l.art_x + i,
                l.art_y + i,
                l.art_size - 2.0 * i,
                l.art_size - 2.0 * i,
                rgba(0, 80, 40, 255),
            );
        }

        let mut ty = l.art_y + (l.info_h * 0.04).floor();
        if let Some(title) = s.title {
            let artist = s.artist.unwrap_or("");
            for (o, a) in [(2.0, 30), (1.0, 55)] {
                g.text(title, l.title_px, l.text_x + o, ty + o, rgba(0, 80, 35, a));
            }
            let (tw, th) = g.text(title, l.title_px, l.text_x, ty, rgba(230, 240, 230, 255));
            ty += th + (l.info_h * 0.04).floor();
            let (aw, ah) = g.text(artist, l.artist_px, l.text_x, ty, rgba(155, 180, 155, 255));
            ty += ah + (l.info_h * 0.02).floor();
            let lw = tw.max(aw);
            g.rect_hgrad(
                l.text_x,
                ty,
                lw,
                1.0,
                rgba(0, 140, 60, 80),
                rgba(0, 140, 60, 0),
            );
            ty += (l.info_h * 0.05).floor();

            let alpha = (160.0 + 95.0 * (frame * 0.04).sin()).floor();
            let dot_r = ((l.small_px / 3).max(4)) as f32;
            let dx = l.text_x + dot_r;
            let dy = ty + dot_r + 1.0;
            g.circle(
                dx,
                dy,
                dot_r * 3.0,
                with_alpha(rgba(0, 180, 70, 0), alpha / 6.0),
            );
            g.circle(
                dx,
                dy,
                (dot_r * 2.0 / 3.0).floor() * 3.0 / 1.5,
                with_alpha(rgba(0, 200, 80, 0), alpha / 4.0),
            );
            g.circle(dx, dy, dot_r, with_alpha(rgba(0, 210, 85, 0), alpha));
            g.text(
                "IDENTIFIED",
                l.small_px,
                l.text_x + dot_r * 2.0 + 6.0,
                ty,
                with_alpha(rgba(0, 180, 70, 0), alpha),
            );
        } else {
            let a = (120.0 + 80.0 * (frame * 0.03).sin()).floor();
            g.text(
                "Listening...",
                l.artist_px,
                l.text_x,
                ty + (l.info_h * 0.2).floor(),
                with_alpha(rgba(100, 120, 100, 0), a),
            );
        }
        let clk = Local::now().format("%H:%M").to_string();
        let (cw, _) = g.measure(&clk, l.small_px);
        g.text(
            &clk,
            l.small_px,
            l.w - cw - (l.w * 0.015).floor(),
            (l.h * 0.02).floor(),
            rgba(100, 120, 100, 255),
        );
    }

    fn draw_spectrum(&mut self, g: &mut Gfx, bars: &[f32; NUM_BARS]) {
        let l = &self.l;
        let y0 = l.info_h + 5.0;
        for i in 1..6 {
            let y = y0 + (l.spec_h * i as f32 / 6.0).floor();
            g.rect(0.0, y, l.w, 1.0, rgba(0, 60, 28, 45));
        }
        for j in 1..10 {
            let x = (l.w * j as f32 / 10.0).floor();
            g.rect(x, y0, 1.0, l.spec_h, rgba(0, 50, 24, 30));
        }
        for i in 1..6 {
            for j in 1..10 {
                let y = y0 + (l.spec_h * i as f32 / 6.0).floor();
                let x = (l.w * j as f32 / 10.0).floor();
                g.circle(x + 0.5, y + 0.5, 2.5, rgba(0, 100, 45, 65));
            }
        }
        let max_h = l.spec_h - 4.0;
        let radius = ((l.bar_w as i32) / 5).max(2) as f32;
        for (i, &b) in bars.iter().enumerate() {
            if b < 0.02 {
                continue;
            }
            let x = l.bar_x0 + i as f32 * (l.bar_w + l.bar_gap);
            let h = (b * max_h).floor().max(2.0);
            let color = bar_color(((b.min(1.0) * 100.0).floor()) / 100.0);
            let r = radius.min((l.bar_w / 2.0).floor()).min((h / 2.0).floor());
            g.bar(x, l.h - h, l.bar_w, h, r, c3(color, 1.0), c3(color, 0.5));
        }
    }
}
