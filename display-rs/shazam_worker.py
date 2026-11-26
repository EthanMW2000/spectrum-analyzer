#!/usr/bin/env python3
"""Long-lived Shazam helper for the Rust display (fallback when Olaf is unsure).

Reads one WAV path per line on stdin, prints one JSON line per request:
{"title": ..., "artist": ...} or null. Runs from ~/hifi-env (shazamio).
"""
import asyncio
import json
import sys

from shazamio import Shazam


async def main():
    shazam = Shazam()
    loop = asyncio.get_running_loop()
    while True:
        line = await loop.run_in_executor(None, sys.stdin.readline)
        if not line:
            break
        out = None
        try:
            result = await shazam.recognize(line.strip())
            track = result.get("track")
            if result.get("matches") and track:
                out = {"title": track.get("title"), "artist": track.get("subtitle")}
        except Exception as e:
            print(f"shazam error: {e}", file=sys.stderr, flush=True)
        print(json.dumps(out), flush=True)


asyncio.run(main())
