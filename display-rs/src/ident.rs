//! Song identification.
//!
//! * Olaf runs as ONE long-lived `olaf_c query` process fed 16 kHz audio over
//!   stdin (no per-second python/ffmpeg/olaf spawns). In stdin mode olaf_c
//!   keeps matches for 10 s and prints the current matches every ~3 s.
//! * If Olaf has nothing confident, a long-lived Shazam helper
//!   (shazam_worker.py) is asked, and its answer is only used when that song
//!   is in collection.json.
//! * A song is only shown after two matches in a row agree.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;

use crate::audio::{Audio, RMS_THRESHOLD};

#[derive(Deserialize, Clone)]
pub struct Record {
    pub title: String,
    pub artist: String,
    #[serde(rename = "coverUrl")]
    pub cover_url: Option<String>,
}

#[derive(Deserialize, Clone)]
struct Track {
    title: String,
    #[serde(rename = "recordId")]
    record_id: String,
}

#[derive(Deserialize, Default)]
struct CollectionFile {
    #[serde(default)]
    tracks: HashMap<String, Track>,
    #[serde(default)]
    records: HashMap<String, Record>,
}

pub struct Collection {
    path: PathBuf,
    mtime: Option<SystemTime>,
    data: CollectionFile,
}

impl Collection {
    pub fn new(path: PathBuf) -> Self {
        let mut c = Collection {
            path,
            mtime: None,
            data: CollectionFile::default(),
        };
        c.refresh();
        c
    }

    /// Reloads collection.json when the hourly sync has rewritten it.
    pub fn refresh(&mut self) {
        let mtime = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();
        if mtime.is_none() || mtime == self.mtime {
            return;
        }
        match std::fs::read(&self.path).map(|b| serde_json::from_slice::<CollectionFile>(&b)) {
            Ok(Ok(d)) => {
                eprintln!(
                    "collection: {} records, {} tracks",
                    d.records.len(),
                    d.tracks.len()
                );
                self.data = d;
                self.mtime = mtime;
            }
            Ok(Err(e)) => eprintln!("collection: bad json: {e}"),
            Err(e) => eprintln!("collection: {e}"),
        }
    }

    pub fn records(&self) -> Vec<Record> {
        self.data.records.values().cloned().collect()
    }

    pub fn song(&self, track_id: &str) -> Option<Song> {
        let t = self.data.tracks.get(track_id)?;
        let r = self.data.records.get(&t.record_id)?;
        Some(Song {
            track_id: track_id.to_string(),
            record_id: t.record_id.clone(),
            title: t.title.clone(),
            artist: r.artist.clone(),
            cover_url: r.cover_url.clone(),
        })
    }

    /// Finds a Shazam answer in the collection. Title must match; the artist
    /// must match too unless the title is long and unique in the collection.
    pub fn find(&self, title: &str, artist: &str) -> Option<String> {
        let qt = norm(title);
        let qa = norm(artist);
        if qt.is_empty() {
            return None;
        }
        let mut exact = Vec::new();
        let mut loose = Vec::new();
        for (id, t) in &self.data.tracks {
            let tt = norm(&t.title);
            if tt == qt {
                exact.push(id);
            } else if tt.len().min(qt.len()) >= 5 && (tt.contains(&qt) || qt.contains(&tt)) {
                loose.push(id);
            }
        }
        let artist_ok = |id: &String| {
            let t = &self.data.tracks[id];
            self.data.records.get(&t.record_id).is_some_and(|r| {
                let ra = norm(&r.artist);
                !qa.is_empty()
                    && !ra.is_empty()
                    && (ra.contains(&qa) || qa.contains(&ra) || shares_word(&ra, &qa))
            })
        };
        let by_artist: Vec<_> = exact.iter().filter(|id| artist_ok(id)).collect();
        if by_artist.len() == 1 {
            return Some((**by_artist[0]).clone());
        }
        if by_artist.is_empty() && exact.len() == 1 && qt.len() >= 8 {
            return Some(exact[0].clone());
        }
        let loose_artist: Vec<_> = loose.iter().filter(|id| artist_ok(id)).collect();
        if exact.is_empty() && loose_artist.len() == 1 {
            return Some((**loose_artist[0]).clone());
        }
        None
    }
}

fn shares_word(a: &str, b: &str) -> bool {
    let stop = ["the", "and", "of", "a", "feat", "ft", "with"];
    a.split(' ')
        .filter(|w| w.len() >= 4 && !stop.contains(w))
        .any(|w| b.split(' ').any(|x| x == w))
}

/// Lowercase, drop "(...)"/"[...]" and " - Remastered" style suffixes, keep letters/digits.
pub fn norm(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    let s = s.to_lowercase();
    let s = s.split(" - ").next().unwrap_or("");
    for c in s.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth > 0 => {}
            '&' => out.push_str(" and "),
            c if c.is_alphanumeric() => out.push(c),
            _ => out.push(' '),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Clone, PartialEq, Debug)]
pub struct Song {
    pub track_id: String,
    pub record_id: String,
    pub title: String,
    pub artist: String,
    pub cover_url: Option<String>,
}

pub struct IdState {
    pub song: Option<Song>,
    pub art: Option<image::RgbaImage>,
    pub art_gen: u64,
}

pub struct Config {
    pub olaf_bin: String,
    pub collection: PathBuf,
    pub shazam_python: PathBuf,
    pub shazam_script: PathBuf,
    pub olaf_confident: u32,
    pub olaf_weak: u32,
    pub art_px: u32,
}

#[derive(Clone)]
pub struct Identifier {
    pub state: Arc<Mutex<IdState>>,
    reset: Arc<AtomicBool>,
}

impl Identifier {
    pub fn start(cfg: Config, audio: Audio, olaf_rx: Receiver<Vec<f32>>) -> Identifier {
        let state = Arc::new(Mutex::new(IdState {
            song: None,
            art: None,
            art_gen: 0,
        }));
        let id = Identifier {
            state,
            reset: Arc::new(AtomicBool::new(false)),
        };
        let me = id.clone();
        let fed = Arc::new(AtomicU64::new(0));
        let (line_tx, line_rx) = mpsc::channel::<String>();
        let stdin: Arc<Mutex<Option<ChildStdin>>> = Arc::new(Mutex::new(None));
        {
            let stdin = stdin.clone();
            let fed = fed.clone();
            std::thread::Builder::new()
                .name("olaf-feed".into())
                .spawn(move || feed_loop(olaf_rx, stdin, fed))
                .unwrap();
        }
        std::thread::Builder::new()
            .name("identify".into())
            .spawn(move || Worker::new(cfg, audio, me, fed, stdin, line_tx, line_rx).run())
            .unwrap();
        id
    }

    /// Called when the display goes idle (new album coming): forget the song.
    pub fn reset(&self) {
        let mut s = self.state.lock().unwrap();
        s.song = None;
        s.art = None;
        s.art_gen += 1;
        self.reset.store(true, Ordering::SeqCst);
        eprintln!("identify: reset for new album");
    }
}

fn feed_loop(rx: Receiver<Vec<f32>>, stdin: Arc<Mutex<Option<ChildStdin>>>, fed: Arc<AtomicU64>) {
    let mut bytes = Vec::new();
    for block in rx {
        let mut guard = stdin.lock().unwrap();
        let Some(w) = guard.as_mut() else { continue };
        bytes.clear();
        for s in &block {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        if w.write_all(&bytes).is_err() {
            *guard = None;
            continue;
        }
        fed.fetch_add(block.len() as u64, Ordering::Relaxed);
    }
}

struct OlafMatch {
    count: u32,
    q_stop: f32,
    track_id: String,
}

fn parse_olaf_line(line: &str) -> Option<OlafMatch> {
    let f: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if f.len() < 7 {
        return None;
    }
    let count = f[0].parse().ok()?;
    let q_stop = f[2].parse().ok()?;
    let track_id = Path::new(f[3]).file_stem()?.to_string_lossy().into_owned();
    Some(OlafMatch {
        count,
        q_stop,
        track_id,
    })
}

/// Two observations in a row of the same track confirm it.
#[derive(Default)]
struct Streak {
    last: Option<String>,
}

impl Streak {
    fn push(&mut self, obs: Option<&str>) -> bool {
        let hit = obs.is_some() && self.last.as_deref() == obs;
        self.last = obs.map(str::to_string);
        hit
    }
}

struct ShazamReq {
    wav: PathBuf,
}

struct Worker {
    cfg: Config,
    audio: Audio,
    id: Identifier,
    fed: Arc<AtomicU64>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    line_tx: Sender<String>,
    line_rx: Receiver<String>,
    olaf: Option<Child>,
    olaf_started: Instant,
    collection: Collection,
    olaf_streak: Streak,
    shazam_streak: Streak,
    olaf_misses: u32,
    weak_candidate: Option<String>,
    shazam_tx: Sender<ShazamReq>,
    shazam_rx: Receiver<Option<(String, String)>>,
    shazam_busy: bool,
    last_shazam: Option<Instant>,
    shazam_backoff: Duration,
    last_block: Instant,
}

const SHAZAM_INTERVAL: Duration = Duration::from_secs(15);

impl Worker {
    fn new(
        cfg: Config,
        audio: Audio,
        id: Identifier,
        fed: Arc<AtomicU64>,
        stdin: Arc<Mutex<Option<ChildStdin>>>,
        line_tx: Sender<String>,
        line_rx: Receiver<String>,
    ) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<ShazamReq>();
        let (res_tx, res_rx) = mpsc::channel();
        let (py, script) = (cfg.shazam_python.clone(), cfg.shazam_script.clone());
        std::thread::Builder::new()
            .name("shazam".into())
            .spawn(move || shazam_loop(py, script, req_rx, res_tx))
            .unwrap();
        let collection = Collection::new(cfg.collection.clone());
        Worker {
            cfg,
            audio,
            id,
            fed,
            stdin,
            line_tx,
            line_rx,
            olaf: None,
            olaf_started: Instant::now(),
            collection,
            olaf_streak: Streak::default(),
            shazam_streak: Streak::default(),
            olaf_misses: 0,
            weak_candidate: None,
            shazam_tx: req_tx,
            shazam_rx: res_rx,
            shazam_busy: false,
            last_shazam: None,
            shazam_backoff: SHAZAM_INTERVAL,
            last_block: Instant::now(),
        }
    }

    fn spawn_olaf(&mut self) {
        // stdbuf makes olaf_c flush each line instead of 4 KB at a time.
        let child = Command::new("stdbuf")
            .args(["-oL", &self.cfg.olaf_bin, "query"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        match child {
            Ok(mut c) => {
                let out = c.stdout.take().unwrap();
                *self.stdin.lock().unwrap() = c.stdin.take();
                self.fed.store(0, Ordering::SeqCst);
                let tx = self.line_tx.clone();
                std::thread::Builder::new()
                    .name("olaf-read".into())
                    .spawn(move || {
                        for line in BufReader::new(out).lines().map_while(Result::ok) {
                            if tx.send(line).is_err() {
                                break;
                            }
                        }
                    })
                    .unwrap();
                eprintln!("olaf: worker started (pid {})", c.id());
                self.olaf = Some(c);
            }
            Err(e) => eprintln!("olaf: cannot start {}: {e}", self.cfg.olaf_bin),
        }
        self.olaf_started = Instant::now();
    }

    fn run(mut self) {
        self.spawn_olaf();
        let mut block: Vec<OlafMatch> = Vec::new();
        let mut in_block = false;
        loop {
            match self.line_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(line) => {
                    if line.starts_with("match count") {
                        if in_block {
                            self.on_block(std::mem::take(&mut block));
                        }
                        in_block = true;
                    } else if let Some(m) = parse_olaf_line(&line) {
                        block.push(m);
                        in_block = true;
                    }
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {
                    if in_block {
                        self.on_block(std::mem::take(&mut block));
                        in_block = false;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {}
            }
            self.housekeeping();
        }
    }

    fn housekeeping(&mut self) {
        if self.id.reset.swap(false, Ordering::SeqCst) {
            self.olaf_streak = Streak::default();
            self.shazam_streak = Streak::default();
            self.olaf_misses = 0;
            self.weak_candidate = None;
            self.shazam_backoff = SHAZAM_INTERVAL;
        }
        let dead = match self.olaf.as_mut().map(|c| c.try_wait()) {
            Some(Ok(None)) => self.stdin.lock().unwrap().is_none(),
            _ => true,
        };
        if dead && self.olaf_started.elapsed() > Duration::from_secs(5) {
            if let Some(mut c) = self.olaf.take() {
                let _ = c.kill();
                let _ = c.wait();
                eprintln!("olaf: worker exited, restarting");
            }
            self.spawn_olaf();
        }
        let playing = self.audio.rms() > RMS_THRESHOLD;
        // olaf_c prints nothing at all when it has no matches.
        if playing && self.last_block.elapsed() > Duration::from_secs(5) {
            self.last_block = Instant::now();
            self.olaf_streak.push(None);
            self.olaf_misses += 1;
            self.weak_candidate = None;
        }
        if let Ok(res) = self.shazam_rx.try_recv() {
            self.shazam_busy = false;
            self.on_shazam(res);
        }
        let due = self
            .last_shazam
            .is_none_or(|t| t.elapsed() > self.shazam_backoff);
        if playing && !self.shazam_busy && self.olaf_misses >= 3 && due {
            let wav = shazam_wav_path();
            let pcm = self.audio.snapshot(10);
            if write_wav(&wav, &pcm).is_ok() && self.shazam_tx.send(ShazamReq { wav }).is_ok() {
                self.shazam_busy = true;
                self.last_shazam = Some(Instant::now());
            }
        }
    }

    fn on_block(&mut self, block: Vec<OlafMatch>) {
        self.last_block = Instant::now();
        self.collection.refresh();
        let now = self.fed.load(Ordering::Relaxed) as f32 / 16000.0;
        let mut best: HashMap<&str, u32> = HashMap::new();
        // Fingerprints whose track is gone from collection.json can't be shown; skip them.
        for m in block
            .iter()
            .filter(|m| m.q_stop >= now - 6.0 && self.collection.song(&m.track_id).is_some())
        {
            let e = best.entry(&m.track_id).or_default();
            *e = (*e).max(m.count);
        }
        let top = best.into_iter().max_by_key(|(_, c)| *c);
        let (obs, weak) = match top {
            Some((t, c)) if c >= self.cfg.olaf_confident => (Some(t.to_string()), None),
            Some((t, c)) if c >= self.cfg.olaf_weak => (None, Some(t.to_string())),
            _ => (None, None),
        };
        if let Some((t, c)) = top {
            eprintln!("olaf: best {t} count={c} ({} entries)", block.len());
        }
        self.weak_candidate = weak;
        if obs.is_some() {
            self.olaf_misses = 0;
            self.shazam_backoff = SHAZAM_INTERVAL;
        } else {
            self.olaf_misses += 1;
        }
        if self.olaf_streak.push(obs.as_deref()) {
            self.confirm(obs.as_deref().unwrap(), "olaf");
        }
    }

    fn on_shazam(&mut self, res: Option<(String, String)>) {
        let Some((title, artist)) = res else {
            eprintln!("shazam: no match");
            self.shazam_streak.push(None);
            return;
        };
        self.collection.refresh();
        let Some(track) = self.collection.find(&title, &artist) else {
            eprintln!("shazam: '{title}' by {artist} is not in the collection, ignoring");
            self.shazam_streak.push(None);
            self.shazam_backoff = Duration::from_secs(30);
            return;
        };
        eprintln!("shazam: '{title}' by {artist} -> track {track}");
        let agrees_with_olaf = self.weak_candidate.as_deref() == Some(track.as_str());
        if self.shazam_streak.push(Some(&track)) || agrees_with_olaf {
            self.confirm(
                &track,
                if agrees_with_olaf {
                    "shazam+olaf"
                } else {
                    "shazam"
                },
            );
        }
        let showing = self
            .id
            .state
            .lock()
            .unwrap()
            .song
            .as_ref()
            .map(|s| s.track_id.clone());
        self.shazam_backoff = if showing.as_deref() == Some(track.as_str()) {
            Duration::from_secs(60)
        } else {
            SHAZAM_INTERVAL
        };
    }

    fn confirm(&mut self, track_id: &str, source: &str) {
        let Some(song) = self.collection.song(track_id) else {
            eprintln!("identify: track {track_id} not in collection.json");
            return;
        };
        let mut st = self.id.state.lock().unwrap();
        if st.song.as_ref().map(|s| &s.track_id) == Some(&song.track_id) {
            return;
        }
        let new_record = st.song.as_ref().map(|s| &s.record_id) != Some(&song.record_id);
        eprintln!("identify: '{}' by {} ({source})", song.title, song.artist);
        st.song = Some(song.clone());
        if new_record {
            st.art = None;
            st.art_gen += 1;
            drop(st);
            let state = self.id.state.clone();
            let px = self.cfg.art_px;
            std::thread::spawn(move || {
                let img = song.cover_url.as_deref().and_then(|u| load_art(u, px));
                let mut st = state.lock().unwrap();
                if st.song.as_ref().map(|s| &s.record_id) == Some(&song.record_id) {
                    st.art = img;
                    st.art_gen += 1;
                }
            });
        }
    }
}

fn shazam_wav_path() -> PathBuf {
    let shm = Path::new("/dev/shm");
    if shm.is_dir() {
        shm.join("hifi-shazam.wav")
    } else {
        std::env::temp_dir().join("hifi-shazam.wav")
    }
}

fn write_wav(path: &Path, pcm: &[i16]) -> hound::Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: crate::audio::RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in pcm {
        w.write_sample(s)?;
    }
    w.finalize()
}

#[derive(Deserialize)]
struct ShazamOut {
    title: Option<String>,
    artist: Option<String>,
}

fn shazam_loop(
    py: PathBuf,
    script: PathBuf,
    rx: Receiver<ShazamReq>,
    tx: Sender<Option<(String, String)>>,
) {
    let mut proc: Option<(Child, ChildStdin, BufReader<std::process::ChildStdout>)> = None;
    for req in rx {
        if proc.is_none() {
            match Command::new(&py)
                .arg(&script)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
            {
                Ok(mut c) => {
                    let i = c.stdin.take().unwrap();
                    let o = BufReader::new(c.stdout.take().unwrap());
                    eprintln!("shazam: helper started (pid {})", c.id());
                    proc = Some((c, i, o));
                }
                Err(e) => {
                    eprintln!("shazam: cannot start {}: {e}", py.display());
                    let _ = tx.send(None);
                    continue;
                }
            }
        }
        let (_, stdin, stdout) = proc.as_mut().unwrap();
        let mut line = String::new();
        let ok = writeln!(stdin, "{}", req.wav.display()).is_ok()
            && stdin.flush().is_ok()
            && stdout.read_line(&mut line).map(|n| n > 0).unwrap_or(false);
        if !ok {
            if let Some((mut c, _, _)) = proc.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            let _ = tx.send(None);
            continue;
        }
        let res = serde_json::from_str::<Option<ShazamOut>>(line.trim())
            .ok()
            .flatten()
            .and_then(|o| Some((o.title?, o.artist.unwrap_or_default())));
        let _ = tx.send(res);
    }
}

fn http() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .build()
        .into()
}

/// Cover art, cached in ~/.olaf/art_cache/<md5(url)> (same cache as the Python version).
pub fn load_art(url: &str, px: u32) -> Option<image::RgbaImage> {
    let dir = home().join(".olaf/art_cache");
    let path = dir.join(format!("{:x}", md5::compute(url.as_bytes())));
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => {
            let b = http().get(url).call().ok()?.body_mut().read_to_vec().ok()?;
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(&path, &b);
            b
        }
    };
    let img = image::load_from_memory(&bytes).ok()?;
    Some(
        img.resize_exact(px, px, image::imageops::FilterType::Triangle)
            .to_rgba8(),
    )
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_olaf_rows() {
        let m = parse_olaf_line(
            "57, 0.16, 3.59, /home/x/vinyl-audio/converted/00856e93-f722.wav, 489242028, 60.16, 63.59",
        )
        .unwrap();
        assert_eq!((m.count, m.track_id.as_str()), (57, "00856e93-f722"));
        assert!(
            parse_olaf_line("match count (#), q start (s) , q stop (s), ref path, ref ID")
                .is_none()
        );
    }

    #[test]
    fn streak_needs_two_in_a_row() {
        let mut s = Streak::default();
        assert!(!s.push(Some("a")));
        assert!(!s.push(None));
        assert!(!s.push(Some("a")));
        assert!(s.push(Some("a")));
        assert!(!s.push(Some("b")));
    }

    #[test]
    fn normalizes_titles() {
        assert_eq!(norm("Blue in Green (Remastered 2009)"), "blue in green");
        assert_eq!(norm("So What - 2019 Remaster"), "so what");
        assert_eq!(norm("Rock & Roll"), "rock and roll");
    }
}
