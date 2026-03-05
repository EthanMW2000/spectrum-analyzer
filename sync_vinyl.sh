#!/bin/bash
set -e

# Pull audio files from S3
aws s3 sync s3://ethanwells-photography/vinyl/audio/ ~/vinyl-audio/

# Convert + ingest into Olaf
for src in ~/vinyl-audio/*.wav ~/vinyl-audio/*.flac ~/vinyl-audio/*.mp3; do
  [ -f "$src" ] || continue
  base=$(basename "$src" | sed 's/\.[^.]*$//')
  wav=~/vinyl-audio/converted/${base}.wav
  if [ ! -f "$wav" ]; then
    mkdir -p ~/vinyl-audio/converted
    ffmpeg -i "$src" -ar 16000 -ac 1 -y "$wav" 2>/dev/null
  fi
  olaf store "$wav"
done

# Pull collection metadata
curl -s -H "x-api-key: $VINYL_IDENTIFY_API_KEY" \
  "$VINYL_IDENTIFY_URL/../export" > ~/.olaf/collection.json

# Back up LMDB to S3
tar -czf /tmp/olaf_db.tar.gz -C ~/.olaf/ .
aws s3 cp /tmp/olaf_db.tar.gz s3://ethanwells-photography/vinyl/fingerprints/olaf_db.tar.gz
rm /tmp/olaf_db.tar.gz

echo "Sync complete"
