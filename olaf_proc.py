#!/usr/bin/env python3
"""
Olaf-based song identification subprocess.
Same interface as shazam_proc.py: receives WAV path, prints JSON to stdout.
Output: { title, artist, art_bytes } or null

Flow:
1. Downsample input WAV to 16kHz mono via ffmpeg
2. Run `olaf query <resampled.wav>`, capture stdout
3. Parse Olaf output → extract matched filename → trackId
4. Look up in ~/.olaf/collection.json → title, artist, coverUrl
5. Fetch album art (cached locally in ~/.olaf/art_cache/)
6. Print JSON result
"""
import sys
import json
import os
import subprocess
import tempfile
import base64
import traceback
import hashlib

COLLECTION_PATH = os.path.expanduser("~/.olaf/collection.json")
ART_CACHE_DIR = os.path.expanduser("~/.olaf/art_cache")


def load_collection():
    if not os.path.exists(COLLECTION_PATH):
        return None
    with open(COLLECTION_PATH) as f:
        return json.load(f)


def downsample(wav_path):
    fd, out_path = tempfile.mkstemp(suffix=".wav")
    os.close(fd)
    try:
        subprocess.run(
            ["ffmpeg", "-i", wav_path, "-ar", "16000", "-ac", "1", "-y", out_path],
            capture_output=True,
            timeout=10,
        )
        return out_path
    except Exception:
        if os.path.exists(out_path):
            os.unlink(out_path)
        raise


def query_olaf(wav_path):
    proc = subprocess.run(
        ["olaf", "query", wav_path],
        capture_output=True,
        text=True,
        timeout=10,
    )
    if proc.returncode != 0:
        print(f"OLAF STDERR: {proc.stderr.strip()}", file=sys.stderr, flush=True)
        return None
    return proc.stdout.strip()


def parse_olaf_output(output):
    """Extract the matched filename from Olaf query output.
    Olaf prints lines like:
        match <filename> <offset> <score> <hash_count>
    We want the filename from the best match.
    """
    best_filename = None
    best_count = 0

    for line in output.splitlines():
        parts = line.strip().split()
        if not parts:
            continue

        if parts[0] == "match" and len(parts) >= 5:
            filename = parts[1]
            try:
                hash_count = int(parts[4])
            except (ValueError, IndexError):
                hash_count = 0
            if hash_count > best_count:
                best_count = hash_count
                best_filename = filename

    if not best_filename:
        return None

    track_id = os.path.splitext(os.path.basename(best_filename))[0]
    return track_id


def fetch_art(cover_url):
    if not cover_url:
        return None

    os.makedirs(ART_CACHE_DIR, exist_ok=True)
    cache_key = hashlib.md5(cover_url.encode()).hexdigest()
    cache_path = os.path.join(ART_CACHE_DIR, cache_key)

    if os.path.exists(cache_path):
        with open(cache_path, "rb") as f:
            return base64.b64encode(f.read()).decode()

    try:
        import requests
        resp = requests.get(cover_url, timeout=5)
        if resp.status_code == 200:
            with open(cache_path, "wb") as f:
                f.write(resp.content)
            return base64.b64encode(resp.content).decode()
    except Exception:
        pass

    return None


def identify(wav_path):
    resampled = downsample(wav_path)
    try:
        output = query_olaf(resampled)
    finally:
        try:
            os.unlink(resampled)
        except OSError:
            pass

    if not output:
        return None

    track_id = parse_olaf_output(output)
    if not track_id:
        return None

    collection = load_collection()
    if not collection:
        print("OLAF: no collection.json found", file=sys.stderr, flush=True)
        return None

    track_info = collection.get("tracks", {}).get(track_id)
    if not track_info:
        print(f"OLAF: trackId {track_id} not in collection", file=sys.stderr, flush=True)
        return None

    record_info = collection.get("records", {}).get(track_info["recordId"])
    if not record_info:
        print(f"OLAF: record {track_info['recordId']} not in collection", file=sys.stderr, flush=True)
        return None

    art_bytes = fetch_art(record_info.get("coverUrl"))

    return {
        "title": track_info["title"],
        "artist": record_info["artist"],
        "art_bytes": art_bytes,
    }


if __name__ == "__main__":
    wav_path = sys.argv[1]
    try:
        result = identify(wav_path)
        print(json.dumps(result))
    except Exception:
        traceback.print_exc()
        print(json.dumps(None))
