# hifi-display (Rust)

GPU-rendered replacement for `hifi_display.py` with the same look and features:
32-band spectrum (same band normalisation and attack/decay), info panel with
album art, idle clock with particles after 30 s of silence, SPIN random album
picker, tap to toggle.

Drawing runs on the Pi 5 GPU through wgpu (Vulkan, V3DV), natively on
labwc/Wayland. The background glow, grain, vignette and scanlines are shaders.

## Song identification

- **Olaf** runs as one long-lived `olaf_c query` process. The display feeds it
  16 kHz audio over stdin, so it no longer starts python + ffmpeg + olaf every
  second. Uses the existing `~/.olaf/db`.
- **Shazam fallback**: when Olaf has had no confident match for ~9 s, the
  long-lived helper `shazam_worker.py` (from `~/hifi-env`) is asked, at most
  every 15 s. Its answer is used only if that song is in
  `~/.olaf/collection.json`.
- A song is shown only after **2 matches in a row** agree (two Olaf reports,
  two Shazam answers, or Shazam agreeing with Olaf's best weak match).

Interfaces it relies on (owned elsewhere): `~/.olaf/db` (Olaf LMDB, filled by
`sync_vinyl.sh`), `~/.olaf/collection.json` (`records{id: {title, artist,
coverUrl}}`, `tracks{id: {title, recordId}}`; reloaded when it changes) and
fingerprint names `<trackId>.wav`.

Tuning (environment variables): `HIFI_RMS_THRESHOLD` (chunk RMS that counts as
music, default 0.02), `HIFI_OLAF_CONFIDENT` (default 12),
`HIFI_OLAF_WEAK` (6), `HIFI_OLAF_BIN`, `HIFI_COLLECTION`, `HIFI_AUDIO_DEVICE`,
`HIFI_FONT`, `HIFI_SHAZAM_PYTHON`, `HIFI_SHAZAM_SCRIPT`.

The input level is logged every 10 s (`level: rms … dBFS, peak … dBFS, …`);
see it with `journalctl -u hifi-display -f | grep level:`.

## Build and deploy (from a Linux PC)

```bash
pip install ziglang cargo-zigbuild
rustup target add aarch64-unknown-linux-gnu
# libasound headers + .so copied from the Pi into a sysroot:
#   ~/pi/sysroot/usr/{include/alsa,lib/aarch64-linux-gnu/libasound.so*,lib/aarch64-linux-gnu/pkgconfig/alsa.pc}
PKG_CONFIG_ALLOW_CROSS=1 PKG_CONFIG_SYSROOT_DIR=~/pi/sysroot \
PKG_CONFIG_LIBDIR=~/pi/sysroot/usr/lib/aarch64-linux-gnu/pkgconfig \
  cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.36
```

Copy `target/aarch64-unknown-linux-gnu/release/hifi-display`,
`shazam_worker.py` and `deploy/*` to `~/spectrum-analyzer-rs/` on the Pi.

## Switching between Rust and Python

The `hifi-display` service stays the same; a systemd drop-in swaps the program.

```bash
~/spectrum-analyzer-rs/switch-display.sh rust     # Rust display
~/spectrum-analyzer-rs/switch-display.sh python   # back to hifi_display.py
```

## Testing without a turntable

```bash
hifi-display --demo                      # fake bars + fake song (like TEST_MODE)
hifi-display --audio-file track.wav      # play a WAV as if it came from the ClearClick
hifi-display --windowed                  # 1280x720 window instead of fullscreen
```
