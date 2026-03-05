#!/usr/bin/env python3
"""
Song identification via fpcalc + portfolio vinyl collection API.
Same subprocess interface as shazam_proc.py: receives WAV path, prints JSON to stdout.
Output: { title, artist, art_bytes } or null
"""
import sys
import json
import base64
import subprocess
import hashlib
import sqlite3
import os
import traceback
from datetime import datetime, timedelta

import requests

API_URL = os.environ["VINYL_IDENTIFY_URL"]
API_KEY = os.environ.get("VINYL_IDENTIFY_API_KEY", "")
CACHE_DB = os.path.expanduser("~/.vinyl_cache.db")
CACHE_TTL_HOURS = 1


def get_cache_db():
    db = sqlite3.connect(CACHE_DB)
    db.execute(
        "CREATE TABLE IF NOT EXISTS cache "
        "(fingerprint_hash TEXT PRIMARY KEY, response TEXT, cached_at TEXT)"
    )
    return db


def cache_lookup(fp_hash):
    try:
        db = get_cache_db()
        row = db.execute(
            "SELECT response, cached_at FROM cache WHERE fingerprint_hash = ?",
            (fp_hash,),
        ).fetchone()
        db.close()
        if not row:
            return None
        cached_at = datetime.fromisoformat(row[1])
        if datetime.now() - cached_at > timedelta(hours=CACHE_TTL_HOURS):
            return None
        return json.loads(row[0])
    except Exception:
        return None


def cache_store(fp_hash, response):
    try:
        db = get_cache_db()
        db.execute(
            "INSERT OR REPLACE INTO cache (fingerprint_hash, response, cached_at) "
            "VALUES (?, ?, ?)",
            (fp_hash, json.dumps(response), datetime.now().isoformat()),
        )
        db.commit()
        db.close()
    except Exception:
        pass


def identify(wav_path):
    proc = subprocess.run(
        ["fpcalc", "-json", wav_path],
        capture_output=True,
        text=True,
        timeout=15,
    )
    if proc.returncode != 0:
        return None

    fpcalc_data = json.loads(proc.stdout)
    fingerprint = fpcalc_data.get("fingerprint")
    duration = fpcalc_data.get("duration", 0)
    if not fingerprint:
        return None

    fp_hash = hashlib.md5(fingerprint.encode()).hexdigest()

    cached = cache_lookup(fp_hash)
    if cached is not None:
        return cached if cached != "null" else None

    resp = requests.post(
        API_URL,
        json={"fingerprint": fingerprint, "duration": duration},
        headers={"x-api-key": API_KEY},
        timeout=10,
    )
    if resp.status_code != 200:
        return None

    data = resp.json()
    match = data.get("match")
    if not match or not match.get("record"):
        cache_store(fp_hash, "null")
        return None

    record = match["record"]
    track = match.get("track") or {}
    title = track.get("title", record.get("title", "Unknown"))
    artist = record.get("artist", "Unknown")

    art_bytes = None
    cover_url = record.get("coverUrl")
    if cover_url:
        try:
            art_resp = requests.get(cover_url, timeout=5)
            if art_resp.status_code == 200:
                art_bytes = base64.b64encode(art_resp.content).decode()
        except Exception:
            pass

    result = {"title": title, "artist": artist, "art_bytes": art_bytes}
    cache_store(fp_hash, result)
    return result


if __name__ == "__main__":
    wav_path = sys.argv[1]
    try:
        result = identify(wav_path)
        print(json.dumps(result))
    except Exception:
        traceback.print_exc()
        print(json.dumps(None))
