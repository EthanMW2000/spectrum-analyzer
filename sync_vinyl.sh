#!/bin/bash
set -e

set -a
source ~/.vinyl.env
set +a

MANIFEST=~/.olaf/stored.txt
touch "$MANIFEST"
mkdir -p ~/vinyl-audio/converted

# List remote tracks, download + convert + ingest only new ones
aws s3api list-objects-v2 --bucket ethanwells-photography --prefix vinyl/audio/ --query 'Contents[].Key' --output text | tr '\t' '\n' | sed 's|vinyl/audio/||' | while read -r filename; do
  [ -z "$filename" ] && continue
  base="${filename%.*}"

  if grep -qxF "$base" "$MANIFEST"; then
    continue
  fi

  echo "New track: $filename"
  aws s3 cp "s3://ethanwells-photography/vinyl/audio/$filename" ~/vinyl-audio/"$filename"

  wav=~/vinyl-audio/converted/${base}.wav
  ffmpeg -i ~/vinyl-audio/"$filename" -ar 16000 -ac 1 -y "$wav" 2>/dev/null
  olaf store "$wav"
  echo "$base" >> "$MANIFEST"
done

# Pull collection metadata
curl -s -H "x-api-key: $VINYL_IDENTIFY_API_KEY" \
  "$VINYL_IDENTIFY_URL/../export" > ~/.olaf/collection.json

# Back up LMDB to S3
tar -czf /tmp/olaf_db.tar.gz -C ~/.olaf/ .
aws s3 cp /tmp/olaf_db.tar.gz s3://ethanwells-photography/vinyl/fingerprints/olaf_db.tar.gz
rm /tmp/olaf_db.tar.gz

echo "Sync complete"
