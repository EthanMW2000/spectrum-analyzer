#!/usr/bin/env python3
"""
Vintage HiFi Display
Real-time spectrum analyser with song identification.
TEST_MODE uses fake audio data; production mode captures via PyAudio.
"""

import pygame
import pygame.gfxdraw
import numpy as np
import time
import math
import random
import os
import sys
import threading
import wave
import io

# ─── MODE / CONFIG ────────────────────────────────────────────────────────────

TEST_MODE          = False
FULLSCREEN         = True
AUDIO_DEVICE_INDEX = 0      # ClearClick USB Audio
RMS_THRESHOLD      = 0.02
IDLE_TIMEOUT       = 30     # seconds of silence before idle screen
IDENTIFY_INTERVAL  = 2      # seconds between identification attempts
IDENTIFY_MODE      = "olaf"  # "olaf" | "shazam" | "shazam+collection"
IDENTIFY_SCRIPTS   = {
    "olaf": "olaf_proc.py",
    "shazam": "shazam_proc.py",
    "shazam+collection": "identify_proc.py",
}
WINDOW_W   = 800    # only used when FULLSCREEN = False
WINDOW_H   = 480    # only used when FULLSCREEN = False

# ─── INIT DISPLAY ─────────────────────────────────────────────────────────────

os.environ['SDL_AUDIODRIVER'] = 'dummy'
pygame.init()

if FULLSCREEN:
    info      = pygame.display.Info()
    DISPLAY_W = info.current_w
    DISPLAY_H = info.current_h
    screen    = pygame.display.set_mode((DISPLAY_W, DISPLAY_H), pygame.FULLSCREEN | pygame.NOFRAME)
else:
    DISPLAY_W = WINDOW_W
    DISPLAY_H = WINDOW_H
    screen    = pygame.display.set_mode((DISPLAY_W, DISPLAY_H))

pygame.display.set_caption("HiFi Display — Test Mode" if TEST_MODE else "HiFi Display")
pygame.mouse.set_visible(not FULLSCREEN)

# ─── FONTS ────────────────────────────────────────────────────────────────────

FONT_PATH = os.path.expanduser("~/.fonts/ShareTechMono.ttf")

def load_font(size):
    if os.path.exists(FONT_PATH):
        return pygame.font.Font(FONT_PATH, size)
    return pygame.font.SysFont('DejaVu Sans Mono', size)

# ─── CONFIG ───────────────────────────────────────────────────────────────────

FPS      = 60
NUM_BARS = 32
BAR_GAP  = max(2, DISPLAY_W // 320)

COLOR_BG           = (14, 18, 15)
COLOR_BAR_LOW      = (0, 230, 95)
COLOR_BAR_MID      = (0, 200, 130)
COLOR_BAR_HIGH     = (200, 245, 200)
COLOR_TEXT_PRIMARY = (230, 240, 230)
COLOR_TEXT_DIM     = (100, 120, 100)
COLOR_TEXT_MID     = (155, 180, 155)
COLOR_DIVIDER      = (45, 60, 45)
COLOR_IDLE_TEXT    = (55, 75, 55)

FONT_TITLE_SIZE  = max(20, DISPLAY_H // 18)
FONT_ARTIST_SIZE = max(16, DISPLAY_H // 24)
FONT_SMALL_SIZE  = max(11, DISPLAY_H // 38)

INFO_H    = int(DISPLAY_H * 0.34)
DIVIDER_Y = INFO_H
SPEC_H    = DISPLAY_H - INFO_H - 5
ART_SIZE  = int(INFO_H * 0.78)
ART_X     = int(DISPLAY_W * 0.025)
ART_Y     = (INFO_H - ART_SIZE) // 2
TEXT_X    = ART_X + ART_SIZE + int(DISPLAY_W * 0.03)

# ─── AUDIO ENGINE (production) ───────────────────────────────────────────────

RATE       = 48000
CHUNK      = 1024
CHANNELS   = 1
FORMAT_PA  = 8   # pyaudio.paInt16

class AudioEngine:
    def __init__(self):
        import pyaudio
        self._pyaudio_mod = pyaudio
        self.pa           = pyaudio.PyAudio()
        self.fft_bars     = np.zeros(NUM_BARS)
        self.rms          = 0.0
        self._lock        = threading.Lock()
        self._last_update = time.time()

        buf_seconds     = 15
        self._buf_len   = RATE * buf_seconds
        self._audio_buf = np.zeros(self._buf_len, dtype=np.int16)
        self._buf_pos   = 0
        self._running   = True
        self._dev_index = AUDIO_DEVICE_INDEX
        if self._dev_index is None:
            self._dev_index = self._find_device()

        self.stream = self._open_stream()
        self._thread = threading.Thread(target=self._read_loop, daemon=True)
        self._thread.start()

    def _open_stream(self):
        return self.pa.open(
            format=self._pyaudio_mod.paInt16,
            channels=CHANNELS,
            rate=RATE,
            input=True,
            input_device_index=self._dev_index,
            frames_per_buffer=CHUNK,
        )

    def _find_device(self):
        for i in range(self.pa.get_device_count()):
            info = self.pa.get_device_info_by_index(i)
            if any(d in info.get('name', '') for d in ('UCA202', 'ClearClick')) and info['maxInputChannels'] > 0:
                return i
        return None

    def _read_loop(self):
        FFT_SIZE    = 2048
        log_edges   = np.logspace(np.log10(30.0), np.log10(8000.0), NUM_BARS + 1)
        freqs       = np.fft.rfftfreq(FFT_SIZE, 1.0 / RATE)
        fft_buf     = np.zeros(FFT_SIZE, dtype=np.float32)
        freq_boost  = np.linspace(1.0, 3.5, NUM_BARS)
        window      = np.hanning(FFT_SIZE)
        error_count = 0

        n_bands     = 6
        band_cuts   = [i * NUM_BARS // n_bands for i in range(1, n_bands)] + [NUM_BARS]
        band_peaks  = np.full(n_bands, 0.01)

        n_fft = len(freqs)
        weights = np.zeros((NUM_BARS, n_fft), dtype=np.float32)
        for i in range(NUM_BARS):
            idx = np.where((freqs >= log_edges[i]) & (freqs < log_edges[i + 1]))[0]
            if len(idx) == 0:
                center = (log_edges[i] + log_edges[i + 1]) / 2.0
                idx = np.array([np.argmin(np.abs(freqs - center))])
            weights[i, idx] = 1.0 / len(idx)

        while self._running:
            try:
                in_data = self.stream.read(CHUNK, exception_on_overflow=False)
                samples = np.frombuffer(in_data, dtype=np.int16).copy()
                error_count = 0

                with self._lock:
                    n   = len(samples)
                    end = self._buf_pos + n
                    if end <= self._buf_len:
                        self._audio_buf[self._buf_pos:end] = samples
                    else:
                        split = self._buf_len - self._buf_pos
                        self._audio_buf[self._buf_pos:] = samples[:split]
                        self._audio_buf[:n - split]     = samples[split:]
                    self._buf_pos = (self._buf_pos + n) % self._buf_len

                floats = samples.astype(np.float32) / 32768.0
                floats = floats - np.mean(floats)
                self.rms = float(np.sqrt(np.mean(floats ** 2)))

                fft_buf[:-CHUNK] = fft_buf[CHUNK:]
                fft_buf[-CHUNK:] = floats

                self._last_update = time.time()

                if self.rms < RMS_THRESHOLD:
                    self.fft_bars = self.fft_bars * 0.93
                    continue

                fft = np.abs(np.fft.rfft(fft_buf * window))

                bars = weights @ fft
                bars = bars * freq_boost

                prev = 0
                for b_idx, b_end in enumerate(band_cuts):
                    band_slice = bars[prev:b_end]
                    peak = np.max(band_slice) if len(band_slice) > 0 else 0.0
                    if peak > band_peaks[b_idx]:
                        band_peaks[b_idx] = band_peaks[b_idx] * 0.5 + peak * 0.5
                    else:
                        band_peaks[b_idx] = band_peaks[b_idx] * 0.97 + peak * 0.03
                    bars[prev:b_end] = np.sqrt(np.clip(
                        band_slice / max(band_peaks[b_idx], 1e-6), 0, 1
                    ))
                    prev = b_end

                attack        = np.where(bars > self.fft_bars, 0.6, 0.0)
                decay         = np.where(bars <= self.fft_bars, 0.07, 0.0)
                self.fft_bars = np.clip(
                    self.fft_bars * (1 - attack - decay) + bars * (attack + decay), 0, 1
                )
            except Exception:
                error_count += 1
                if error_count > 20:
                    try:
                        self.stream.stop_stream()
                        self.stream.close()
                    except Exception:
                        pass
                    try:
                        self.stream = self._open_stream()
                        error_count = 0
                    except Exception:
                        time.sleep(1)
                else:
                    time.sleep(0.01)
                continue

    def update(self):
        pass

    def get_bars(self):
        return self.fft_bars.copy()

    def get_rms(self):
        return self.rms

    def get_audio_buffer_wav(self):
        with self._lock:
            pos = self._buf_pos
            buf = np.concatenate((self._audio_buf[pos:], self._audio_buf[:pos]))
        bio = io.BytesIO()
        with wave.open(bio, 'wb') as wf:
            wf.setnchannels(CHANNELS)
            wf.setsampwidth(2)
            wf.setframerate(RATE)
            wf.writeframes(buf.tobytes())
        return bio.getvalue()

    def close(self):
        self._running = False
        self._thread.join(timeout=2)
        self.stream.stop_stream()
        self.stream.close()
        self.pa.terminate()


# ─── SHAZAM WORKER (production) ─────────────────────────────────────────────

class IdentifyWorker:
    def __init__(self, audio_engine):
        self.audio    = audio_engine
        self._state   = {
            'title':  None,
            'artist': None,
            'art':    None,
            'bg':     None,
            'status': 'Listening...',
        }
        self._lock    = threading.Lock()
        self._running = True
        self._pending_title = None
        self._pending_count = 0
        self._thread  = threading.Thread(target=self._run, daemon=True)
        self._thread.start()
        tag = IDENTIFY_MODE.upper()
        print(f"{tag}: worker started", flush=True)

    def _run(self):
        import subprocess
        import json
        import base64
        import tempfile

        script_name = IDENTIFY_SCRIPTS[IDENTIFY_MODE]
        script = os.path.join(os.path.dirname(os.path.abspath(__file__)), script_name)
        tag = IDENTIFY_MODE.upper()

        while self._running:
            time.sleep(IDENTIFY_INTERVAL)
            if not self._running:
                break
            try:
                if self.audio.get_rms() < RMS_THRESHOLD:
                    continue
                print(f"{tag}: attempting identification...", flush=True)
                wav_bytes = self.audio.get_audio_buffer_wav()

                with tempfile.NamedTemporaryFile(suffix='.wav', delete=False) as tmp:
                    tmp.write(wav_bytes)
                    tmp_path = tmp.name

                try:
                    proc = subprocess.run(
                        [sys.executable, script, tmp_path],
                        capture_output=True, text=True, timeout=30,
                    )
                finally:
                    try:
                        os.unlink(tmp_path)
                    except OSError:
                        pass

                if proc.stderr:
                    print(f"{tag} STDERR: {proc.stderr.strip()}", flush=True)
                if proc.returncode != 0:
                    print(f"{tag}: subprocess exited with code {proc.returncode}", flush=True)
                    continue

                stdout = proc.stdout.strip()
                print(f"{tag}: got response ({len(stdout)} bytes)", flush=True)
                result = json.loads(stdout)
                if result is None:
                    with self._lock:
                        if self._state['title'] is None:
                            self._state['status'] = 'Listening...'
                    continue

                candidate = result['title']
                if candidate == self._pending_title:
                    self._pending_count += 1
                else:
                    self._pending_title = candidate
                    self._pending_count = 1

                already_showing = self._state.get('title') == candidate
                if self._pending_count < 2 and not already_showing:
                    print(f"{tag}: candidate '{candidate}' by {result['artist']} (need one more match)", flush=True)
                    continue

                art_bytes = None
                if result.get('art_bytes'):
                    art_bytes = base64.b64decode(result['art_bytes'])

                with self._lock:
                    artist_changed = self._state.get('artist') and self._state['artist'] != result['artist']
                    self._state['title']  = result['title']
                    self._state['artist'] = result['artist']
                    self._state['status'] = 'Identified'
                    if art_bytes and (artist_changed or not self._state.get('art')):
                        self._state['art'] = None
                        self._state['_art_bytes'] = art_bytes
                    print(f"{tag}: '{result['title']}' by {result['artist']}", flush=True)
            except Exception as e:
                print(f"{tag} WORKER: {e}", flush=True)

    def get_state(self):
        with self._lock:
            state = dict(self._state)

        if state.get('_art_bytes') and state.get('art') is None:
            try:
                img_io   = io.BytesIO(state['_art_bytes'])
                img      = pygame.image.load(img_io)
                art_surf = pygame.transform.smoothscale(img, (ART_SIZE, ART_SIZE))
                with self._lock:
                    self._state['art'] = art_surf
                    self._state.pop('_art_bytes', None)
                state['art'] = art_surf
            except Exception:
                pass
        state.pop('_art_bytes', None)
        return state

    def reset(self):
        with self._lock:
            self._state = {
                'title': None, 'artist': None,
                'art': None, 'bg': None,
                'status': 'Listening...',
            }
        tag = IDENTIFY_MODE.upper()
        print(f"{tag}: reset for new album", flush=True)

    def stop(self):
        self._running = False

# ─── PARTICLES ────────────────────────────────────────────────────────────────

class Particle:
    def __init__(self):
        self.reset(initial=True)

    def reset(self, initial=False):
        self.x              = random.uniform(0, DISPLAY_W)
        self.y              = random.uniform(0, DISPLAY_H) if initial else DISPLAY_H + 10
        self.size           = random.uniform(0.8, 2.2)
        self.speed          = random.uniform(0.08, 0.32)
        self.vx             = random.uniform(-0.1, 0.1)
        self.vy             = -self.speed
        self.alpha          = random.randint(25, 120)
        self.flicker_speed  = random.uniform(0.01, 0.04)
        self.flicker_offset = random.uniform(0, math.pi * 2)
        self.draw_alpha     = self.alpha
        self.life           = 0
        self.max_life       = random.randint(400, 1200)

    def update(self, frame):
        self.x    += self.vx + math.sin(frame * 0.01 + self.flicker_offset) * 0.12
        self.y    += self.vy
        self.life += 1
        flicker         = math.sin(frame * self.flicker_speed + self.flicker_offset)
        self.draw_alpha = int(max(0, min(160, self.alpha + flicker * 20)))
        if self.life > self.max_life * 0.8:
            fade            = 1.0 - (self.life - self.max_life * 0.8) / (self.max_life * 0.2)
            self.draw_alpha = int(self.draw_alpha * fade)
        if self.life >= self.max_life or self.y < -20:
            self.reset()

    def draw(self, surf):
        r = max(1, int(self.size))
        pygame.gfxdraw.filled_circle(surf, int(self.x), int(self.y), r,
                                     (0, 200, 80, self.draw_alpha))
        if r >= 1:
            pygame.gfxdraw.filled_circle(surf, int(self.x), int(self.y), r + 1,
                                         (0, 200, 80, self.draw_alpha // 5))

class ParticleSystem:
    def __init__(self, count=100):
        self.particles = [Particle() for _ in range(count)]
        self.frame     = 0

    def update_and_draw(self, target):
        self.frame += 1
        for p in self.particles:
            p.update(self.frame)
            p.draw(target)

# ─── BACKGROUND SURFACES ──────────────────────────────────────────────────────

def make_vignette():
    surf = pygame.Surface((DISPLAY_W, DISPLAY_H), pygame.SRCALPHA)
    cx, cy = DISPLAY_W // 2, DISPLAY_H // 2
    max_r  = math.hypot(cx, cy)
    for r in range(int(max_r), 0, -6):
        alpha = int(50 * (r / max_r) ** 2.2)
        pygame.gfxdraw.filled_circle(surf, cx, cy, r, (0, 0, 0, alpha))
    return surf

def make_grain():
    np.random.seed(42)
    arr        = np.zeros((DISPLAY_H, DISPLAY_W, 3), dtype=np.uint8)
    noise      = np.random.randint(0, 14, (DISPLAY_H, DISPLAY_W), dtype=np.uint8)
    arr[:,:,1] = noise // 2   # faint green tint
    arr[:,:,0] = noise // 8
    surf       = pygame.surfarray.make_surface(arr.swapaxes(0, 1))
    surf.set_alpha(28)
    return surf

def make_scanlines():
    surf = pygame.Surface((DISPLAY_W, DISPLAY_H), pygame.SRCALPHA)
    for y in range(0, DISPLAY_H, 2):
        pygame.draw.line(surf, (0, 0, 0, 12), (0, y), (DISPLAY_W, y))
    return surf

def make_background_glow():
    cx, cy = DISPLAY_W / 2, DISPLAY_H / 2
    max_dist = math.hypot(cx, cy)
    xs = np.arange(DISPLAY_W)
    ys = np.arange(DISPLAY_H)
    dx = (xs[np.newaxis, :] - cx) / max_dist
    dy = (ys[:, np.newaxis] - cy) / max_dist
    dist = np.sqrt(dx ** 2 + dy ** 2)
    falloff = np.exp(-dist ** 2 * 3.5)
    arr = np.zeros((DISPLAY_H, DISPLAY_W, 4), dtype=np.uint8)
    arr[:, :, 1] = (falloff * 80).astype(np.uint8)
    arr[:, :, 2] = (falloff * 35).astype(np.uint8)
    arr[:, :, 3] = (falloff * 95).astype(np.uint8)
    surf = pygame.image.frombuffer(arr.tobytes(), (DISPLAY_W, DISPLAY_H), "RGBA")
    return surf.convert_alpha()

# ─── HELPERS ──────────────────────────────────────────────────────────────────

def aa_line(surf, color, p1, p2, width=1):
    if width <= 1:
        pygame.gfxdraw.line(surf, int(p1[0]), int(p1[1]), int(p2[0]), int(p2[1]), color)
    else:
        x1, y1 = p1; x2, y2 = p2
        dx, dy  = x2 - x1, y2 - y1
        length  = math.hypot(dx, dy)
        if length == 0:
            return
        ux, uy  = dx / length, dy / length
        px, py  = -uy * width / 2, ux * width / 2
        pts     = [(x1+px, y1+py), (x2+px, y2+py), (x2-px, y2-py), (x1-px, y1-py)]
        pts_int = [(int(x), int(y)) for x, y in pts]
        pygame.gfxdraw.filled_polygon(surf, pts_int, color)
        pygame.gfxdraw.aapolygon(surf, pts_int, color)

def make_bar_mask(w, h, radius):
    mask = pygame.Surface((w, h), pygame.SRCALPHA)
    pygame.draw.rect(mask, (255, 255, 255, 255), (0, 0, w, h),
                     border_top_left_radius=radius, border_top_right_radius=radius)
    return mask

def make_scope_grid():
    surf = pygame.Surface((DISPLAY_W, SPEC_H), pygame.SRCALPHA)
    h_lines = 6
    v_lines = 10
    for i in range(1, h_lines):
        y = int(SPEC_H * i / h_lines)
        pygame.draw.line(surf, (0, 60, 28, 45), (0, y), (DISPLAY_W, y))
    for i in range(1, v_lines):
        x = int(DISPLAY_W * i / v_lines)
        pygame.draw.line(surf, (0, 50, 24, 30), (x, 0), (x, SPEC_H))
    for i in range(1, h_lines):
        for j in range(1, v_lines):
            y = int(SPEC_H * i / h_lines)
            x = int(DISPLAY_W * j / v_lines)
            pygame.gfxdraw.filled_circle(surf, x, y, 2, (0, 100, 45, 65))
    return surf

# ─── RENDERER ─────────────────────────────────────────────────────────────────

class Renderer:
    def __init__(self, screen):
        self.screen    = screen
        self.particles = ParticleSystem(count=100)
        self.vignette  = make_vignette()
        self.grain     = make_grain()
        self.scanlines = make_scanlines()
        self.bg_glow    = make_background_glow()
        self.scope_grid = make_scope_grid()

        self.font_title  = load_font(FONT_TITLE_SIZE)
        self.font_artist = load_font(FONT_ARTIST_SIZE)
        self.font_small  = load_font(FONT_SMALL_SIZE)

        self.font_clock_big  = load_font(int(DISPLAY_H * 0.28))
        self.font_clock_date = load_font(int(DISPLAY_H * 0.06))

        self.bar_w       = (DISPLAY_W - (NUM_BARS - 1) * BAR_GAP) // NUM_BARS
        self.bar_x_start = (DISPLAY_W - (self.bar_w * NUM_BARS + BAR_GAP * (NUM_BARS - 1))) // 2
        self.frame       = 0
        self._bar_cache  = {}
        self._cache_max  = 256

    def draw(self, bars, song_state, idle):
        self.screen.fill(COLOR_BG)
        self.screen.blit(self.bg_glow, (0, 0))
        self.screen.blit(self.grain,   (0, 0))
        if idle:
            self._draw_idle()
        else:
            self._draw_info_panel(song_state)
            self._draw_divider()
            self._draw_spectrum(bars)
        self.screen.blit(self.vignette,  (0, 0))
        self.screen.blit(self.scanlines, (0, 0))
        self.frame += 1

    # ── Idle ───────────────────────────────────────────────────────────────────

    def _draw_idle(self):
        self.particles.update_and_draw(self.screen)
        cx  = DISPLAY_W // 2
        now = time.localtime()
        colon    = ":" if now.tm_sec % 2 == 0 else " "
        time_str = time.strftime(f"%H{colon}%M")

        # Layered phosphor glow
        for ox, oy, alpha in [(3, 3, 12), (2, 2, 22), (1, 1, 38)]:
            g = self.font_clock_big.render(time_str, True, (0, 90, 35))
            g.set_alpha(alpha)
            self.screen.blit(g, (cx - g.get_width()//2 + ox,
                                 DISPLAY_H//2 - g.get_height()//2 - int(DISPLAY_H*0.05) + oy))

        time_surf = self.font_clock_big.render(time_str, True, (0, 220, 90))
        time_x    = cx - time_surf.get_width() // 2
        time_y    = DISPLAY_H // 2 - time_surf.get_height() // 2 - int(DISPLAY_H * 0.05)
        self.screen.blit(time_surf, (time_x, time_y))

        sec_surf = self.font_clock_date.render(time.strftime(":%S"), True, (0, 130, 55))
        self.screen.blit(sec_surf, (
            time_x + time_surf.get_width() + int(DISPLAY_W * 0.005),
            time_y + time_surf.get_height() - sec_surf.get_height() - int(DISPLAY_H * 0.01)))

        date_surf = self.font_clock_date.render(time.strftime("%A,  %B %-d  %Y"), True, (60, 110, 70))
        self.screen.blit(date_surf, (
            cx - date_surf.get_width() // 2,
            time_y + time_surf.get_height() + int(DISPLAY_H * 0.02)))

    # ── HiFi panel ─────────────────────────────────────────────────────────────

    def _draw_info_panel(self, s):
        self._draw_art_box(s)

        text_y = ART_Y + int(INFO_H * 0.04)

        if s['title']:
            for ox, oy, a in [(2, 2, 30), (1, 1, 55)]:
                glow = self.font_title.render(s['title'], True, (0, 80, 35))
                glow.set_alpha(a)
                self.screen.blit(glow, (TEXT_X + ox, text_y + oy))
            title_surf = self.font_title.render(s['title'], True, COLOR_TEXT_PRIMARY)
            self.screen.blit(title_surf, (TEXT_X, text_y))
            text_y += title_surf.get_height() + int(INFO_H * 0.04)

            artist_surf = self.font_artist.render(s['artist'], True, COLOR_TEXT_MID)
            self.screen.blit(artist_surf, (TEXT_X, text_y))
            text_y += artist_surf.get_height() + int(INFO_H * 0.02)

            line_w = max(artist_surf.get_width(), title_surf.get_width())
            line_surf = pygame.Surface((line_w, 1), pygame.SRCALPHA)
            for px in range(line_w):
                fade = 1.0 - (px / line_w)
                line_surf.set_at((px, 0), (0, 140, 60, int(80 * fade)))
            self.screen.blit(line_surf, (TEXT_X, text_y))
            text_y += int(INFO_H * 0.05)

            alpha   = int(160 + 95 * math.sin(self.frame * 0.04))
            dot_r   = max(4, FONT_SMALL_SIZE // 3)
            dot_x   = TEXT_X + dot_r
            dot_y   = text_y + dot_r + 1
            glow_r  = dot_r * 3
            glow_size = glow_r * 2 + 2
            glow_surf = pygame.Surface((glow_size, glow_size), pygame.SRCALPHA)
            gc = glow_r + 1
            pygame.draw.circle(glow_surf, (0, 180, 70, alpha // 6), (gc, gc), glow_r)
            pygame.draw.circle(glow_surf, (0, 200, 80, alpha // 4), (gc, gc), glow_r * 2 // 3)
            pygame.draw.circle(glow_surf, (0, 210, 85, alpha), (gc, gc), dot_r)
            self.screen.blit(glow_surf, (dot_x - gc, dot_y - gc))

            id_surf = self.font_small.render("IDENTIFIED", True, (0, 180, 70))
            id_surf.set_alpha(alpha)
            self.screen.blit(id_surf, (TEXT_X + dot_r * 2 + 6, text_y))
        else:
            st = self.font_artist.render(s['status'], True, COLOR_TEXT_DIM)
            st.set_alpha(int(120 + 80 * math.sin(self.frame * 0.03)))
            self.screen.blit(st, (TEXT_X, text_y + int(INFO_H * 0.2)))

        clk = self.font_small.render(time.strftime("%H:%M"), True, COLOR_TEXT_DIM)
        self.screen.blit(clk, (DISPLAY_W - clk.get_width() - int(DISPLAY_W * 0.015),
                               int(DISPLAY_H * 0.02)))

    def _draw_art_box(self, s):
        pygame.draw.rect(self.screen, (10, 14, 11), (ART_X, ART_Y, ART_SIZE, ART_SIZE))

        if s.get('art'):
            self.screen.blit(s['art'], (ART_X, ART_Y))
        else:
            cx = ART_X + ART_SIZE // 2
            cy = ART_Y + ART_SIZE // 2
            wave_bars = 16
            bar_w = max(2, ART_SIZE // (wave_bars * 2))
            total_w = wave_bars * bar_w * 2
            start_x = cx - total_w // 2
            for i in range(wave_bars):
                t = self.frame * 0.05
                h = int(ART_SIZE * 0.12 * (
                    0.3 + 0.7 * abs(math.sin(t + i * 0.5))
                ) * (0.5 + 0.5 * math.sin(t * 0.7 + i * 0.3)))
                h = max(2, h)
                bx = start_x + i * bar_w * 2
                fade = 1.0 - abs(i - wave_bars / 2) / (wave_bars / 2)
                a = int(40 + 50 * fade)
                pygame.draw.rect(self.screen, (0, 120, 55, a),
                                 (bx, cy - h // 2, bar_w, h))

        for inset in range(3):
            a = 30 - inset * 10
            pygame.draw.rect(self.screen, (0, 80, 40, a),
                             (ART_X + inset, ART_Y + inset,
                              ART_SIZE - inset * 2, ART_SIZE - inset * 2), 1)

    def _draw_divider(self):
        pygame.draw.line(self.screen, COLOR_DIVIDER, (0, DIVIDER_Y), (DISPLAY_W, DIVIDER_Y), 1)

    def _get_bar_surf(self, h, color_idx):
        key = (h, color_idx)
        cached = self._bar_cache.get(key)
        if cached:
            return cached

        if len(self._bar_cache) > self._cache_max:
            self._bar_cache.clear()

        color = self._bar_color(color_idx / 100.0)
        radius = max(2, self.bar_w // 5)

        bar_surf = pygame.Surface((self.bar_w, h), pygame.SRCALPHA)
        for row in range(h):
            bf = 1.0 - (row / h) * 0.5
            row_color = tuple(min(255, int(c * bf)) for c in color) + (255,)
            pygame.draw.line(bar_surf, row_color, (0, row), (self.bar_w - 1, row))

        r = min(radius, self.bar_w // 2, h // 2)
        if r > 1:
            mask = make_bar_mask(self.bar_w, h, r)
            bar_surf.blit(mask, (0, 0), special_flags=pygame.BLEND_RGBA_MIN)

        self._bar_cache[key] = bar_surf
        return bar_surf

    @staticmethod
    def _bar_color(val):
        if val < 0.5:
            t = val / 0.5
            return tuple(int(a + (b - a) * t) for a, b in zip(COLOR_BAR_LOW, COLOR_BAR_MID))
        t = (val - 0.5) / 0.5
        return tuple(int(a + (b - a) * t) for a, b in zip(COLOR_BAR_MID, COLOR_BAR_HIGH))

    def _draw_spectrum(self, bars):
        max_h  = SPEC_H - 4
        base_y = DISPLAY_H

        self.screen.blit(self.scope_grid, (0, DIVIDER_Y + 5))

        for i, bar in enumerate(bars):
            if bar < 0.02:
                continue
            x = self.bar_x_start + i * (self.bar_w + BAR_GAP)
            h = max(2, int(bar * max_h))
            color_idx = int(min(bar, 1.0) * 100)

            bar_surf = self._get_bar_surf(h, color_idx)
            self.screen.blit(bar_surf, (x, base_y - h))


# ─── MAIN ─────────────────────────────────────────────────────────────────────

def main():
    clock  = pygame.time.Clock()
    render = Renderer(screen)

    if TEST_MODE:
        from fake_audio import FakeAudioEngine, FAKE_SONG
        audio      = FakeAudioEngine(num_bars=NUM_BARS)
        song_state = FAKE_SONG
        identifier     = None
        idle       = False
    else:
        audio      = AudioEngine()
        identifier     = IdentifyWorker(audio)
        song_state = None
        idle       = False

    last_sound_time  = time.time()
    last_tap_time    = 0.0
    TAP_LOCK_SECONDS = 8

    running = True
    while running:
        try:
            for event in pygame.event.get():
                if event.type == pygame.QUIT:
                    running = False
                if event.type == pygame.KEYDOWN:
                    if event.key == pygame.K_ESCAPE:
                        running = False
                if event.type in (pygame.MOUSEBUTTONDOWN, pygame.FINGERDOWN):
                    idle          = not idle
                    last_tap_time = time.time()
                    if not idle:
                        last_sound_time = time.time()

            audio.update()

            if not TEST_MODE:
                tap_locked = time.time() - last_tap_time < TAP_LOCK_SECONDS
                song_state = identifier.get_state()

                stale = time.time() - audio._last_update
                if stale > 3.0:
                    audio.fft_bars = audio.fft_bars * 0.9
                    if stale > 6.0:
                        try:
                            audio.stream.stop_stream()
                            audio.stream.close()
                        except Exception:
                            pass
                        try:
                            audio.stream = audio._open_stream()
                            audio._last_update = time.time()
                            if not audio._thread.is_alive():
                                audio._thread = threading.Thread(target=audio._read_loop, daemon=True)
                                audio._thread.start()
                        except Exception:
                            pass

                if not tap_locked:
                    if audio.get_rms() > RMS_THRESHOLD:
                        last_sound_time = time.time()
                        idle = False
                    elif time.time() - last_sound_time > IDLE_TIMEOUT:
                        if not idle:
                            identifier.reset()
                        idle = True

            render.draw(audio.get_bars(), song_state, idle)
            pygame.display.flip()
            clock.tick(FPS)
        except KeyboardInterrupt:
            running = False
        except Exception as e:
            print(f"MAIN LOOP ERROR: {e}", flush=True)
            time.sleep(0.1)

    if identifier:
        identifier.stop()
    if not TEST_MODE:
        audio.close()
    pygame.quit()

if __name__ == '__main__':
    main()