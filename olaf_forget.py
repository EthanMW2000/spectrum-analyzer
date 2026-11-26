#!/usr/bin/env python3
"""Remove tracks from the Olaf LMDB index without their audio.

`olaf delete` re-fingerprints the audio file to find what to remove, so it only
works while the exact stored file still exists. sync_vinyl.sh deletes audio after
fingerprinting, so this removes every fingerprint and the metadata entry whose
audio id belongs to the given identifiers instead.

Usage: olaf_forget.py IDENTIFIER...   (identifier = the path given to `olaf store`)

Olaf's LMDB layout (src/olaf_db.c): "olaf_fingerprints" maps hash -> (t1 << 32) + audio_id
as integer dups; "olaf_resource_map" maps audio_id -> {float duration; long fingerprints;
char path[512]}.
"""
import os
import struct
import sys

import lmdb

DB_DIR = os.environ.get("OLAF_DB_DIR", os.path.expanduser("~/.olaf/db"))
PATH_OFFSET = 16
ID_MASK = 0xFFFFFFFF


def main(identifiers):
    wanted = {os.fsencode(i) for i in identifiers}
    env = lmdb.open(DB_DIR, max_dbs=2, map_size=1 << 40)
    fps = env.open_db(b"olaf_fingerprints", integerkey=True, dupsort=True,
                      dupfixed=True, integerdup=True, create=False)
    meta = env.open_db(b"olaf_resource_map", integerkey=True, create=False)

    with env.begin(write=True) as txn:
        ids = {}
        for key, value in txn.cursor(meta):
            path = value[PATH_OFFSET:].split(b"\0", 1)[0]
            if path in wanted:
                ids[struct.unpack("=I", key)[0]] = key
        if not ids:
            return

        stale = [(key, value) for key, value in txn.cursor(fps)
                 if struct.unpack("=Q", value)[0] & ID_MASK in ids]
        for key, value in stale:
            txn.delete(key, value, db=fps)

        for key in ids.values():
            txn.delete(key, db=meta)

    print(f"Removed {len(ids)} track(s), {len(stale)} fingerprints")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    main(sys.argv[1:])
