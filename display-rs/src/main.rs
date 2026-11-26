//! HiFi display: GPU-rendered phosphor spectrum analyser with song ID.
//!
//! Usage: hifi-display [--demo] [--windowed] [--audio-file x.wav] [--device plughw:1,0]

mod audio;
mod gfx;
mod ident;
mod ui;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

use audio::{Audio, Source, rms_threshold};
use ident::{Collection, Identifier};
use ui::{IMG_ART, IMG_PICK, SongView, Ui};

const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const TAP_LOCK: Duration = Duration::from_secs(8);

struct Args {
    demo: bool,
    windowed: bool,
    audio_file: Option<PathBuf>,
    device: Option<String>,
}

fn parse_args() -> Args {
    let mut a = Args {
        demo: false,
        windowed: false,
        audio_file: None,
        device: std::env::var("HIFI_AUDIO_DEVICE").ok(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--demo" => a.demo = true,
            "--windowed" => a.windowed = true,
            "--audio-file" => a.audio_file = it.next().map(PathBuf::from),
            "--device" => a.device = it.next(),
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    a
}

fn env_or(key: &str, default: impl Into<String>) -> String {
    std::env::var(key).unwrap_or_else(|_| default.into())
}

fn load_font() -> Vec<u8> {
    let home = ident::home();
    let candidates = [
        std::env::var("HIFI_FONT")
            .map(PathBuf::from)
            .unwrap_or_default(),
        home.join(".fonts/ShareTechMono.ttf"),
        PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"),
    ];
    for p in candidates.iter().filter(|p| !p.as_os_str().is_empty()) {
        if let Ok(b) = std::fs::read(p) {
            eprintln!("font: {}", p.display());
            return b;
        }
    }
    panic!("no font found (set HIFI_FONT)");
}

struct App {
    args: Args,
    audio: Audio,
    ident: Option<Identifier>,
    collection: Collection,
    gfx: Option<gfx::Gfx>,
    ui: Option<Ui>,
    idle: bool,
    last_sound: Instant,
    last_tap: Option<Instant>,
    cursor: (f32, f32),
    art_gen: u64,
    pick_gen: u64,
    has_art: bool,
}

impl App {
    fn tap(&mut self, x: f32, y: f32) {
        let Some(ui) = &self.ui else { return };
        if self.idle
            && ui
                .btn
                .is_some_and(|(bx, by, bw, bh)| x >= bx && x < bx + bw && y >= by && y < by + bh)
        {
            self.collection.refresh();
            ui.spin(self.collection.records());
            return;
        }
        if self.idle {
            ui.clear_pick();
        }
        self.idle = !self.idle;
        self.last_tap = Some(Instant::now());
        if !self.idle {
            self.last_sound = Instant::now();
        }
    }

    fn frame(&mut self) {
        let (Some(g), Some(ui)) = (self.gfx.as_mut(), self.ui.as_mut()) else {
            return;
        };
        if !self.args.demo {
            let tap_locked = self.last_tap.is_some_and(|t| t.elapsed() < TAP_LOCK);
            if self.audio.stale_secs() > 3.0 {
                self.audio.decay(0.9);
            }
            if !tap_locked {
                if self.audio.rms() > rms_threshold() {
                    self.last_sound = Instant::now();
                    self.idle = false;
                    ui.clear_pick();
                } else if self.last_sound.elapsed() > IDLE_TIMEOUT {
                    if !self.idle
                        && let Some(id) = &self.ident
                    {
                        id.reset();
                    }
                    self.idle = true;
                }
            }
        }

        let (title, artist) = match &self.ident {
            None => (
                Some("Blue in Green".to_string()),
                Some("Miles Davis".to_string()),
            ),
            Some(id) => {
                let st = id.state.lock().unwrap();
                if st.art_gen != self.art_gen {
                    self.art_gen = st.art_gen;
                    self.has_art = st.art.is_some();
                    if let Some(img) = &st.art {
                        g.set_image(IMG_ART, img);
                    }
                }
                (
                    st.song.as_ref().map(|s| s.title.clone()),
                    st.song.as_ref().map(|s| s.artist.clone()),
                )
            }
        };
        {
            let p = ui.pick.lock().unwrap();
            if p.generation != self.pick_gen {
                self.pick_gen = p.generation;
                if let Some(img) = &p.art {
                    g.set_image(IMG_PICK, img);
                }
                ui.pick_shown = p.art.is_some();
            }
        }
        let view = SongView {
            title: title.as_deref(),
            artist: artist.as_deref(),
            has_art: self.has_art,
        };
        let bars = self.audio.bars();
        g.begin();
        ui.draw(g, &bars, &view, self.idle);
        g.end();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let mut attrs = Window::default_attributes().with_title("HiFi Display");
        if self.args.windowed {
            attrs = attrs.with_inner_size(winit::dpi::LogicalSize::new(1280, 720));
        } else {
            attrs = attrs
                .with_fullscreen(Some(Fullscreen::Borderless(None)))
                .with_decorations(false);
        }
        let window = Arc::new(el.create_window(attrs).expect("window"));
        if !self.args.windowed {
            window.set_cursor_visible(false);
        }
        let g = gfx::Gfx::new(window, load_font());
        self.ui = Some(Ui::new(g.width, g.height));
        eprintln!("display: {}x{}", g.width, g.height);
        g.window.request_redraw();
        self.gfx = Some(g);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape)
                {
                    el.exit();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(g) = self.gfx.as_mut() {
                    g.resize(size.width, size.height);
                    if let Some(ui) = self.ui.as_mut() {
                        ui.resize(g.width, g.height);
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as f32, position.y as f32)
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => {
                let (x, y) = self.cursor;
                self.tap(x, y);
            }
            WindowEvent::Touch(t) if t.phase == TouchPhase::Started => {
                self.tap(t.location.x as f32, t.location.y as f32);
            }
            WindowEvent::RedrawRequested => {
                self.frame();
                if let Some(g) = &self.gfx {
                    g.window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let args = parse_args();
    let home = ident::home();
    let collection_path = PathBuf::from(env_or(
        "HIFI_COLLECTION",
        home.join(".olaf/collection.json").to_string_lossy(),
    ));
    if !args.demo {
        eprintln!("audio: music threshold {} RMS", rms_threshold());
    }
    let (audio, ident) = if args.demo {
        (Audio::start(Source::Fake, None), None)
    } else {
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let source = match &args.audio_file {
            Some(p) => Source::File(p.clone()),
            None => Source::Alsa(args.device.clone()),
        };
        let audio = Audio::start(source, Some(tx));
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_default();
        let cfg = ident::Config {
            olaf_bin: env_or("HIFI_OLAF_BIN", "olaf_c"),
            collection: collection_path.clone(),
            shazam_python: PathBuf::from(env_or(
                "HIFI_SHAZAM_PYTHON",
                home.join("hifi-env/bin/python3").to_string_lossy(),
            )),
            shazam_script: PathBuf::from(env_or(
                "HIFI_SHAZAM_SCRIPT",
                exe_dir.join("shazam_worker.py").to_string_lossy(),
            )),
            olaf_confident: env_or("HIFI_OLAF_CONFIDENT", "12").parse().unwrap_or(12),
            olaf_weak: env_or("HIFI_OLAF_WEAK", "6").parse().unwrap_or(6),
            art_px: 512,
        };
        let id = Identifier::start(cfg, audio.clone(), rx);
        (audio, Some(id))
    };
    let mut app = App {
        args,
        audio,
        ident,
        collection: Collection::new(collection_path),
        gfx: None,
        ui: None,
        idle: false,
        last_sound: Instant::now(),
        last_tap: None,
        cursor: (0.0, 0.0),
        art_gen: 0,
        pick_gen: 0,
        has_art: false,
    };
    let el = EventLoop::new().expect("event loop");
    el.set_control_flow(ControlFlow::Wait);
    el.run_app(&mut app).expect("run");
}
