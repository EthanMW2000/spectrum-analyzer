//! Audio capture, the 32-band spectrum and the 16 kHz feed for Olaf.
//!
//! The band maths is a straight port of `AudioEngine._read_loop` in
//! hifi_display.py, so the bars move exactly like the Python version.

use std::path::PathBuf;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use realfft::RealFftPlanner;

pub const RATE: u32 = 48000;
pub const CHUNK: usize = 1024;
pub const NUM_BARS: usize = 32;
const DEFAULT_RMS_THRESHOLD: f32 = 0.02;
const LEVEL_LOG_SECS: usize = 10;

/// Chunk RMS above which audio counts as music (env `HIFI_RMS_THRESHOLD`, 0 < t < 1, default 0.02).
pub fn rms_threshold() -> f32 {
    static T: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *T.get_or_init(|| {
        std::env::var("HIFI_RMS_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|t: &f32| *t > 0.0 && *t < 1.0)
            .unwrap_or(DEFAULT_RMS_THRESHOLD)
    })
}

fn dbfs(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

/// Input level over the last ~10 s, logged so the signal chain can be tuned.
#[derive(Default)]
struct LevelMeter {
    sum_sq: f64,
    samples: usize,
    raw_peak: [f32; 2],
    max_chunk_rms: f32,
    chunks: usize,
    chunks_above: usize,
}

impl LevelMeter {
    /// `raw_peak` is the per-channel peak before downmix and DC removal, so clipping shows.
    fn add(&mut self, floats: &[f32], chunk_rms: f32, raw_peak: [f32; 2]) {
        for &f in floats {
            self.sum_sq += (f * f) as f64;
        }
        for (mine, new) in self.raw_peak.iter_mut().zip(raw_peak) {
            *mine = mine.max(new);
        }
        self.samples += floats.len();
        self.max_chunk_rms = self.max_chunk_rms.max(chunk_rms);
        self.chunks += 1;
        if chunk_rms >= rms_threshold() {
            self.chunks_above += 1;
        }
        if self.samples >= LEVEL_LOG_SECS * RATE as usize {
            let rms = (self.sum_sq / self.samples as f64).sqrt() as f32;
            eprintln!(
                "level: rms {:.1} dBFS, peak L {:.1} R {:.1} dBFS, loudest chunk {:.1} dBFS, {}/{} chunks above threshold {} ({:.1} dBFS)",
                dbfs(rms),
                dbfs(self.raw_peak[0]),
                dbfs(self.raw_peak[1]),
                dbfs(self.max_chunk_rms),
                self.chunks_above,
                self.chunks,
                rms_threshold(),
                dbfs(rms_threshold()),
            );
            *self = LevelMeter::default();
        }
    }
}
const FFT_SIZE: usize = 2048;
const RING_SECS: usize = 12;

pub enum Source {
    Alsa(Option<String>),
    File(PathBuf),
    Fake,
}

struct Shared {
    bars: [f32; NUM_BARS],
    rms: f32,
    last_update: Instant,
    ring: Vec<i16>,
    ring_pos: usize,
}

#[derive(Clone)]
pub struct Audio {
    shared: Arc<Mutex<Shared>>,
}

impl Audio {
    pub fn start(source: Source, olaf_tx: Option<SyncSender<Vec<f32>>>) -> Audio {
        let shared = Arc::new(Mutex::new(Shared {
            bars: [0.0; NUM_BARS],
            rms: 0.0,
            last_update: Instant::now(),
            ring: vec![0; RATE as usize * RING_SECS],
            ring_pos: 0,
        }));
        let audio = Audio { shared };
        let a = audio.clone();
        std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || match source {
                Source::Fake => fake_loop(a),
                Source::File(p) => {
                    let mut dsp = Dsp::new(a, olaf_tx);
                    file_loop(&p, &mut dsp)
                }
                Source::Alsa(dev) => {
                    let mut dsp = Dsp::new(a, olaf_tx);
                    alsa_loop(dev, &mut dsp)
                }
            })
            .expect("spawn audio thread");
        audio
    }

    pub fn bars(&self) -> [f32; NUM_BARS] {
        self.shared.lock().unwrap().bars
    }

    pub fn rms(&self) -> f32 {
        self.shared.lock().unwrap().rms
    }

    pub fn stale_secs(&self) -> f32 {
        self.shared
            .lock()
            .unwrap()
            .last_update
            .elapsed()
            .as_secs_f32()
    }

    pub fn decay(&self, f: f32) {
        let mut s = self.shared.lock().unwrap();
        for b in s.bars.iter_mut() {
            *b *= f;
        }
    }

    /// The last `secs` seconds of mono 48 kHz audio, oldest first.
    pub fn snapshot(&self, secs: usize) -> Vec<i16> {
        let s = self.shared.lock().unwrap();
        let n = (secs * RATE as usize).min(s.ring.len());
        let len = s.ring.len();
        let start = (s.ring_pos + len - n) % len;
        (0..n).map(|i| s.ring[(start + i) % len]).collect()
    }
}

/// Low-pass FIR + take every 3rd sample: 48 kHz -> 16 kHz for Olaf.
struct Decimator {
    taps: Vec<f32>,
    hist: Vec<f32>,
    pos: usize,
    phase: usize,
}

impl Decimator {
    fn new() -> Self {
        let n = 63;
        let fc = 7000.0 / RATE as f32;
        let m = (n - 1) as f32 / 2.0;
        let mut taps: Vec<f32> = (0..n)
            .map(|i| {
                let x = i as f32 - m;
                let sinc = if x == 0.0 {
                    2.0 * fc
                } else {
                    (2.0 * std::f32::consts::PI * fc * x).sin() / (std::f32::consts::PI * x)
                };
                let w =
                    0.54 - 0.46 * (2.0 * std::f32::consts::PI * i as f32 / (n - 1) as f32).cos();
                sinc * w
            })
            .collect();
        let sum: f32 = taps.iter().sum();
        taps.iter_mut().for_each(|t| *t /= sum);
        Decimator {
            hist: vec![0.0; n],
            taps,
            pos: 0,
            phase: 0,
        }
    }

    fn push(&mut self, x: f32, out: &mut Vec<f32>) {
        let n = self.hist.len();
        self.hist[self.pos] = x;
        self.pos = (self.pos + 1) % n;
        self.phase += 1;
        if self.phase == 3 {
            self.phase = 0;
            let mut acc = 0.0;
            for (k, t) in self.taps.iter().enumerate() {
                acc += t * self.hist[(self.pos + k) % n];
            }
            out.push(acc);
        }
    }
}

struct Dsp {
    audio: Audio,
    olaf_tx: Option<SyncSender<Vec<f32>>>,
    dec: Decimator,
    fft: Arc<dyn realfft::RealToComplex<f32>>,
    fft_buf: Vec<f32>,
    fft_in: Vec<f32>,
    fft_out: Vec<realfft::num_complex::Complex<f32>>,
    window: Vec<f32>,
    weights: Vec<Vec<(usize, f32)>>,
    freq_boost: [f32; NUM_BARS],
    band_cuts: Vec<usize>,
    band_peaks: Vec<f32>,
    meter: LevelMeter,
}

impl Dsp {
    fn new(audio: Audio, olaf_tx: Option<SyncSender<Vec<f32>>>) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let fft_in = fft.make_input_vec();
        let fft_out = fft.make_output_vec();
        let window = (0..FFT_SIZE)
            .map(|n| {
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * n as f32 / (FFT_SIZE - 1) as f32).cos()
            })
            .collect();
        let n_fft = FFT_SIZE / 2 + 1;
        let freqs: Vec<f32> = (0..n_fft)
            .map(|k| k as f32 * RATE as f32 / FFT_SIZE as f32)
            .collect();
        let (lo, hi) = (30f32.log10(), 8000f32.log10());
        let edges: Vec<f32> = (0..=NUM_BARS)
            .map(|i| 10f32.powf(lo + (hi - lo) * i as f32 / NUM_BARS as f32))
            .collect();
        let weights = (0..NUM_BARS)
            .map(|i| {
                let mut idx: Vec<usize> = (0..n_fft)
                    .filter(|&k| freqs[k] >= edges[i] && freqs[k] < edges[i + 1])
                    .collect();
                if idx.is_empty() {
                    let c = (edges[i] + edges[i + 1]) / 2.0;
                    let best = (0..n_fft)
                        .min_by(|&a, &b| (freqs[a] - c).abs().total_cmp(&(freqs[b] - c).abs()))
                        .unwrap();
                    idx.push(best);
                }
                let w = 1.0 / idx.len() as f32;
                idx.into_iter().map(|k| (k, w)).collect()
            })
            .collect();
        let mut freq_boost = [0.0; NUM_BARS];
        for (i, f) in freq_boost.iter_mut().enumerate() {
            *f = 1.0 + 2.5 * i as f32 / (NUM_BARS - 1) as f32;
        }
        let n_bands = 6;
        let mut band_cuts: Vec<usize> = (1..n_bands).map(|i| i * NUM_BARS / n_bands).collect();
        band_cuts.push(NUM_BARS);
        Dsp {
            audio,
            olaf_tx,
            dec: Decimator::new(),
            fft,
            fft_buf: vec![0.0; FFT_SIZE],
            fft_in,
            fft_out,
            window,
            weights,
            freq_boost,
            band_cuts,
            band_peaks: vec![0.01; n_bands],
            meter: LevelMeter::default(),
        }
    }

    fn process(&mut self, samples: &[i16], raw_peak: [f32; 2]) {
        if let Some(tx) = &self.olaf_tx {
            let mut out = Vec::with_capacity(samples.len() / 3 + 1);
            for &s in samples {
                self.dec.push(s as f32 / 32768.0, &mut out);
            }
            let _ = tx.try_send(out);
        }

        let n = samples.len();
        let mean = samples.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / n as f32;
        let floats: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0 - mean).collect();
        let rms = (floats.iter().map(|f| f * f).sum::<f32>() / n as f32).sqrt();
        self.meter.add(&floats, rms, raw_peak);

        self.fft_buf.copy_within(n.., 0);
        let len = self.fft_buf.len();
        self.fft_buf[len - n..].copy_from_slice(&floats);

        let mut sh = self.audio.shared.lock().unwrap();
        for &s in samples {
            let p = sh.ring_pos;
            sh.ring[p] = s;
            sh.ring_pos = (p + 1) % sh.ring.len();
        }
        sh.rms = rms;
        sh.last_update = Instant::now();
        if rms < rms_threshold() {
            for b in sh.bars.iter_mut() {
                *b *= 0.93;
            }
            return;
        }
        let prev_bars = sh.bars;
        drop(sh);

        for i in 0..FFT_SIZE {
            self.fft_in[i] = self.fft_buf[i] * self.window[i];
        }
        let _ = self.fft.process(&mut self.fft_in, &mut self.fft_out);
        let mut bars = [0f32; NUM_BARS];
        for (i, b) in bars.iter_mut().enumerate() {
            *b = self.weights[i]
                .iter()
                .map(|&(k, w)| w * self.fft_out[k].norm())
                .sum::<f32>()
                * self.freq_boost[i];
        }
        let mut prev = 0;
        for (bi, &end) in self.band_cuts.iter().enumerate() {
            let peak = bars[prev..end].iter().cloned().fold(0.0, f32::max);
            let bp = &mut self.band_peaks[bi];
            if peak > *bp {
                *bp = *bp * 0.5 + peak * 0.5;
            } else {
                *bp = *bp * 0.97 + peak * 0.03;
            }
            let div = bp.max(1e-6);
            for b in &mut bars[prev..end] {
                *b = (*b / div).clamp(0.0, 1.0).sqrt();
            }
            prev = end;
        }
        let mut out = [0f32; NUM_BARS];
        for i in 0..NUM_BARS {
            let k = if bars[i] > prev_bars[i] { 0.6 } else { 0.07 };
            out[i] = (prev_bars[i] * (1.0 - k) + bars[i] * k).clamp(0.0, 1.0);
        }
        self.audio.shared.lock().unwrap().bars = out;
    }
}

fn find_card(want: &Option<String>) -> Option<String> {
    if let Some(d) = want {
        return Some(d.clone());
    }
    for card in alsa::card::Iter::new().flatten() {
        let name = card.get_name().unwrap_or_default();
        let long = card.get_longname().unwrap_or_default();
        if ["ClearClick", "UCA202"]
            .iter()
            .any(|n| name.contains(n) || long.contains(n))
        {
            return Some(format!("plughw:{},0", card.get_index()));
        }
    }
    None
}

fn open_pcm(dev: &str) -> alsa::Result<alsa::PCM> {
    use alsa::pcm::{Access, Format, HwParams};
    let pcm = alsa::PCM::new(dev, alsa::Direction::Capture, true)?;
    {
        let hwp = HwParams::any(&pcm)?;
        hwp.set_channels(2)?;
        hwp.set_rate(RATE, alsa::ValueOr::Nearest)?;
        hwp.set_format(Format::s16())?;
        hwp.set_access(Access::RWInterleaved)?;
        hwp.set_period_size_near(CHUNK as alsa::pcm::Frames, alsa::ValueOr::Nearest)?;
        hwp.set_buffer_size_near((CHUNK * 8) as alsa::pcm::Frames)?;
        pcm.hw_params(&hwp)?;
    }
    pcm.start()?;
    Ok(pcm)
}

fn alsa_loop(dev: Option<String>, dsp: &mut Dsp) {
    let mut buf = vec![0i16; CHUNK * 2];
    let mut mono = Vec::with_capacity(CHUNK);
    loop {
        let Some(name) = find_card(&dev) else {
            eprintln!("audio: no ClearClick/UCA202 capture device, retrying");
            std::thread::sleep(Duration::from_secs(2));
            continue;
        };
        let pcm = match open_pcm(&name) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("audio: open {name}: {e}");
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        eprintln!("audio: capturing from {name}");
        let io = pcm.io_i16().expect("i16 io");
        let mut last_data = Instant::now();
        loop {
            match pcm.wait(Some(500)) {
                Ok(true) => {}
                Ok(false) => {
                    if last_data.elapsed() > Duration::from_secs(3) {
                        eprintln!("audio: no data for 3 s, reopening");
                        break;
                    }
                    continue;
                }
                Err(e) => {
                    if pcm.try_recover(e, true).is_err() {
                        break;
                    }
                    continue;
                }
            }
            match io.readi(&mut buf) {
                Ok(frames) if frames > 0 => {
                    last_data = Instant::now();
                    mono.clear();
                    let mut peak = [0i32; 2];
                    for f in 0..frames {
                        let l = buf[2 * f] as i32;
                        let r = buf[2 * f + 1] as i32;
                        peak = [peak[0].max(l.abs()), peak[1].max(r.abs())];
                        mono.push(((l + r) / 2) as i16);
                    }
                    dsp.process(&mono, peak.map(|p| p as f32 / 32768.0));
                }
                Ok(_) => {}
                Err(e) if e.errno() == libc_eagain() => {}
                Err(e) => {
                    if pcm.try_recover(e, true).is_err() {
                        eprintln!("audio: read error {e}, reopening");
                        break;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

const fn libc_eagain() -> i32 {
    11
}

/// Plays a WAV file as if it came from the turntable (looped, real time).
fn file_loop(path: &PathBuf, dsp: &mut Dsp) {
    // --audio-file is a test mode: a bad file should stop the program, not leave it silent.
    let fail = |msg: String| -> ! {
        eprintln!("audio: {}: {msg}", path.display());
        std::process::exit(1)
    };
    let mut reader = hound::WavReader::open(path).unwrap_or_else(|e| fail(e.to_string()));
    let spec = reader.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<Result<_, _>>()
        }
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>(),
    }
    .unwrap_or_else(|e| fail(e.to_string()));
    let ch = spec.channels as usize;
    let mono: Vec<f32> = raw
        .chunks(ch)
        .map(|c| c.iter().sum::<f32>() / ch as f32)
        .collect();
    if mono.is_empty() {
        fail("no audio samples".into());
    }
    let ratio = spec.sample_rate as f64 / RATE as f64;
    let out_len = (mono.len() as f64 / ratio) as usize;
    let pcm: Vec<i16> = (0..out_len)
        .map(|i| {
            let x = i as f64 * ratio;
            let j = x as usize;
            let t = (x - j as f64) as f32;
            let a = mono[j.min(mono.len() - 1)];
            let b = mono[(j + 1).min(mono.len() - 1)];
            ((a + (b - a) * t) * 32767.0) as i16
        })
        .collect();
    eprintln!(
        "audio: playing {} ({:.0} s, looped)",
        path.display(),
        pcm.len() as f32 / RATE as f32
    );
    let start = Instant::now();
    let mut sent = 0usize;
    loop {
        let off = sent % pcm.len();
        let end = (off + CHUNK).min(pcm.len());
        let chunk = &pcm[off..end];
        let peak = chunk.iter().map(|&s| (s as i32).abs()).max().unwrap_or(0) as f32 / 32768.0;
        dsp.process(chunk, [peak; 2]);
        sent += end - off;
        let due = Duration::from_secs_f64(sent as f64 / RATE as f64);
        if let Some(d) = due.checked_sub(start.elapsed()) {
            std::thread::sleep(d);
        }
    }
}

/// Same synthetic bars as fake_audio.py, for `--demo`.
fn fake_loop(a: Audio) {
    let mut frame = 0u64;
    let mut bars = [0f32; NUM_BARS];
    let mut seed = 0x2545F4914F6CDD1Du64;
    loop {
        frame += 1;
        let t = frame as f32 * 0.04;
        let mut target = [0f32; NUM_BARS];
        for (i, v) in target.iter_mut().enumerate() {
            let pos = i as f32 / NUM_BARS as f32;
            let bass = 0.8 * (-(pos - 0.08).powi(2) / 0.01).exp() * (0.6 + 0.4 * (t * 1.7).sin());
            let mid =
                0.5 * (-(pos - 0.35).powi(2) / 0.02).exp() * (0.5 + 0.5 * (t * 2.3 + 1.0).sin());
            let high =
                0.25 * (-(pos - 0.75).powi(2) / 0.04).exp() * (0.4 + 0.6 * (t * 3.1 + 2.0).sin());
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let noise = (seed % 1000) as f32 / 1000.0 * 0.06;
            *v = (bass + mid + high + noise).min(1.0);
        }
        for i in 0..NUM_BARS {
            let k = if target[i] > bars[i] { 0.6 } else { 0.07 };
            bars[i] = (bars[i] * (1.0 - k) + target[i] * k).clamp(0.0, 1.0);
        }
        {
            let mut s = a.shared.lock().unwrap();
            s.bars = bars;
            s.rms = bars.iter().sum::<f32>() / NUM_BARS as f32;
            s.last_update = Instant::now();
        }
        std::thread::sleep(Duration::from_micros(16_667));
    }
}
