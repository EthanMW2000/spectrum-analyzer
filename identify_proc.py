#!/usr/bin/env python3
"""
Song identification via Shazam + portfolio vinyl collection API.
Same subprocess interface as shazam_proc.py: receives WAV path, prints JSON to stdout.
Output: { title, artist, art_bytes } or null

When a song is identified by Shazam, checks the portfolio API to see if it
exists in the vinyl collection. If so, returns the collection's album art
instead of Shazam's.
"""
import sys
import json
import base64
import os
import traceback

import requests

API_URL = os.environ["VINYL_IDENTIFY_URL"]
API_KEY = os.environ.get("VINYL_IDENTIFY_API_KEY", "")


def shazam_identify(wav_bytes):
    import asyncio
    from shazamio import Shazam

    async def _run():
        result = await Shazam().recognize(wav_bytes)
        matches = result.get('matches', [])
        track = result.get('track')
        if not matches or not track:
            return None

        return {
            'title': track.get('title', 'Unknown'),
            'artist': track.get('subtitle', 'Unknown'),
        }

    return asyncio.run(_run())


def check_collection(title, artist):
    try:
        resp = requests.post(
            API_URL,
            json={"title": title, "artist": artist},
            headers={"x-api-key": API_KEY},
            timeout=10,
        )
        if resp.status_code != 200:
            return None

        data = resp.json()
        return data.get("match")
    except Exception:
        return None


def identify(wav_path):
    with open(wav_path, 'rb') as f:
        wav_bytes = f.read()

    shazam_result = shazam_identify(wav_bytes)
    if not shazam_result:
        return None

    title = shazam_result['title']
    artist = shazam_result['artist']

    match = check_collection(title, artist)

    art_bytes = None
    if match and match.get('record', {}).get('coverUrl'):
        try:
            art_resp = requests.get(match['record']['coverUrl'], timeout=5)
            if art_resp.status_code == 200:
                art_bytes = base64.b64encode(art_resp.content).decode()
        except Exception:
            pass

    if match and match.get('track'):
        title = match['track']['title']

    return {'title': title, 'artist': artist, 'art_bytes': art_bytes}


if __name__ == '__main__':
    wav_path = sys.argv[1]
    try:
        result = identify(wav_path)
        print(json.dumps(result))
    except Exception:
        traceback.print_exc()
        print(json.dumps(None))
