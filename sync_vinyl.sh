#!/bin/bash
# Hourly (cron): fingerprint new or changed S3 audio into Olaf, refresh
# collection.json, and back up the Olaf DB to S3 when it changed.
#
# S3 layout: s3://$BUCKET/vinyl/audio/{trackId}.{ext}. A track is re-ingested
# when its object's key or ETag changes (if several objects exist for one
# trackId, the newest wins). Audio is deleted once fingerprinted.
set -euo pipefail

OLAF_DIR=~/.olaf
mkdir -p "$OLAF_DIR"
exec 9>"$OLAF_DIR/sync.lock"
flock -n 9 || exit 0

set -a
# shellcheck source=/dev/null
source ~/.vinyl.env
set +a

BUCKET=ethanwells-photography
PREFIX=vinyl/audio/
MANIFEST=$OLAF_DIR/stored.tsv     # trackId <TAB> s3 key <TAB> etag
LEGACY_MANIFEST=$OLAF_DIR/stored.txt
WORK=~/vinyl-audio
CONVERTED=$WORK/converted
SCRIPT_DIR=$(dirname "$(readlink -f "$0")")
S3CP=(aws s3 cp --only-show-errors)

log() { echo "$(date '+%F %T') $*"; }

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$CONVERTED"
touch "$MANIFEST"

# trackId <TAB> key <TAB> etag <TAB> lastModified, newest object per trackId
aws s3api list-objects-v2 --bucket "$BUCKET" --prefix "$PREFIX" \
  --query 'Contents[].[Key,ETag,LastModified]' --output text > "$TMP/list.raw"
awk -F'\t' -v OFS='\t' -v p="$PREFIX" '
  NF >= 3 {
    file = substr($1, length(p) + 1)
    if (file == "" || index(file, "/")) next
    id = file; sub(/\.[^.]*$/, "", id)
    gsub(/"/, "", $2)
    print id, $1, $2, $3
  }' "$TMP/list.raw" | sort -t$'\t' -k1,1 -k4,4r | awk -F'\t' '!seen[$1]++' > "$TMP/remote.tsv"

# One-time migration from stored.txt (trackIds only): adopt the current S3
# object unless it is newer than the local copy that was fingerprinted.
if [ ! -s "$MANIFEST" ] && [ -s "$LEGACY_MANIFEST" ]; then
  while IFS=$'\t' read -r id key etag modified; do
    grep -qxF "$id" "$LEGACY_MANIFEST" || continue
    local_copy="$WORK/${key#"$PREFIX"}"
    if [ -f "$local_copy" ] && [ "$(date -d "$modified" +%s)" -gt "$(stat -c %Y "$local_copy")" ]; then
      continue
    fi
    printf '%s\t%s\t%s\n' "$id" "$key" "$etag" >> "$MANIFEST"
  done < "$TMP/remote.tsv"
  log "Migrated $(wc -l < "$MANIFEST") tracks from stored.txt"
fi

awk -F'\t' -v OFS='\t' '
  FILENAME == ARGV[1] { stored[$1] = $2 "\t" $3; next }
  stored[$1] != $2 "\t" $3 { print $1, $2 }' "$MANIFEST" "$TMP/remote.tsv" > "$TMP/todo.tsv"

changed=0
if [ -s "$TMP/todo.tsv" ]; then
  log "$(wc -l < "$TMP/todo.tsv") new or changed track(s)"

  # Drop old fingerprints first: `olaf store` skips identifiers already indexed.
  mapfile -t wavs < <(cut -f1 "$TMP/todo.tsv" | sed "s|.*|$CONVERTED/&.wav|")
  python3 "$SCRIPT_DIR/olaf_forget.py" "${wavs[@]}"
  awk -F'\t' 'FILENAME == ARGV[1] { todo[$1]; next } !($1 in todo)' "$TMP/todo.tsv" "$MANIFEST" > "$TMP/manifest"
  cat "$TMP/manifest" > "$MANIFEST"
  changed=1

  ingest() {
    local id=$1 key=$2 src="$WORK/${2#"$PREFIX"}" wav="$CONVERTED/$1.wav"
    "${S3CP[@]}" "s3://$BUCKET/$key" "$src" || return 1
    ffmpeg -nostdin -loglevel error -i "$src" -ar 16000 -ac 1 -y "$wav" || return 1
    olaf store "$wav" > /dev/null || return 1
  }

  while IFS=$'\t' read -r id key; do
    etag=$(awk -F'\t' -v id="$id" '$1 == id { print $3 }' "$TMP/remote.tsv")
    if ingest "$id" "$key"; then
      printf '%s\t%s\t%s\n' "$id" "$key" "$etag" >> "$MANIFEST"
      log "Stored $key"
    else
      log "FAILED $key (will retry next run)"
    fi
    rm -f "$WORK/${key#"$PREFIX"}" "$CONVERTED/$id.wav"
  done < "$TMP/todo.tsv"

  cut -f1 "$MANIFEST" > "$LEGACY_MANIFEST"
fi

# Pull collection metadata; keep the old copy if the request fails.
if curl -fsS --max-time 60 -H "x-api-key: $VINYL_IDENTIFY_API_KEY" \
     "$VINYL_IDENTIFY_URL/../export" -o "$TMP/collection.json" \
   && python3 -c 'import json, sys; json.load(open(sys.argv[1]))' "$TMP/collection.json"; then
  cmp -s "$TMP/collection.json" "$OLAF_DIR/collection.json" \
    || mv "$TMP/collection.json" "$OLAF_DIR/collection.json"
else
  log "Collection export failed; keeping previous collection.json"
fi

if [ "$changed" = 1 ]; then
  tar -czf "$TMP/olaf_db.tar.gz" -C "$OLAF_DIR" ./db ./stored.txt ./stored.tsv ./collection.json
  "${S3CP[@]}" "$TMP/olaf_db.tar.gz" "s3://$BUCKET/vinyl/fingerprints/olaf_db.tar.gz"
  log "Backed up Olaf DB"
fi
