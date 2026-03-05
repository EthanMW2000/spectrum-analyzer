#!/usr/bin/env python3
import sys
import json
import base64
import traceback


def shazam_identify(wav_bytes):
    import asyncio
    from shazamio import Shazam
    import requests as req

    async def _run():
        result = await Shazam().recognize(wav_bytes)
        matches = result.get('matches', [])
        track   = result.get('track')
        if not matches or not track:
            return None

        title  = track.get('title', 'Unknown')
        artist = track.get('subtitle', 'Unknown')
        images = track.get('images', {})
        art_url = images.get('coverart') or images.get('coverarthq')

        art_bytes = None
        if art_url:
            try:
                resp = req.get(art_url, timeout=5)
                if resp.status_code == 200:
                    art_bytes = base64.b64encode(resp.content).decode()
            except Exception:
                pass

        return {'title': title, 'artist': artist, 'art_bytes': art_bytes}

    return asyncio.run(_run())


if __name__ == '__main__':
    wav_path = sys.argv[1]
    try:
        with open(wav_path, 'rb') as f:
            wav_bytes = f.read()
        result = shazam_identify(wav_bytes)
        print(json.dumps(result))
    except Exception:
        traceback.print_exc()
        print(json.dumps(None))
