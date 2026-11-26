#!/bin/bash
# Hourly (cron): fingerprint new or changed S3 audio into Olaf, refresh
# collection.json, and back up the Olaf DB to S3 when it changed.
#
# S3 layout: s3://$BUCKET/vinyl/audio/{trackId}.{ext}. A track is re-ingested
# when its object's key or ETag changes (if several objects exist for one
# trackId, the newest wins). Audio is deleted once fingerprinted.
set -euo pipefail
export PATH=/usr/local/bin:$PATH  # cron PATH lacks olaf

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
BACKUP_KEY=vinyl/fingerprints/olaf_db.tar.gz
# No local Olaf DB: restore the S3 backup (a rebuild that hits one bad track
# would back up a partial index over it). Rebuild only when no backup exists.
if [ ! -s "$OLAF_DIR/db/data.mdb" ]; then
  if [ "$(aws s3api list-objects-v2 --bucket "$BUCKET" --prefix "$BACKUP_KEY" --query KeyCount --output text)" = 0 ]; then
    log "No Olaf DB or S3 backup; fingerprinting every track"
    rm -f "$MANIFEST" "$LEGACY_MANIFEST"
    : > "$MANIFEST"
  else
    "${S3CP[@]}" "s3://$BUCKET/$BACKUP_KEY" "$TMP/restore.tar.gz"
    tar -xzf "$TMP/restore.tar.gz" -C "$OLAF_DIR"
    log "Olaf DB missing; restored it from s3://$BUCKET/$BACKUP_KEY"
  fi
fi

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
if [ ! -e "$MANIFEST" ] && [ -s "$LEGACY_MANIFEST" ]; then
  while IFS=$'\t' read -r id key etag modified; do
    grep -qxF "$id" "$LEGACY_MANIFEST" || continue
    local_copy=
    for f in "$WORK/$id".*; do
      if [ -f "$f" ] && [ "$f" -nt "${local_copy:-/nonexistent}" ]; then local_copy=$f; fi
    done
    if [ -f "$local_copy" ] && [ "$(date -d "$modified" +%s)" -gt "$(stat -c %Y "$local_copy")" ]; then
      continue
    fi
    printf '%s\t%s\t%s\n' "$id" "$key" "$etag" >> "$MANIFEST"
  done < "$TMP/remote.tsv"
  log "Migrated $(wc -l < "$MANIFEST") tracks from stored.txt"
fi
touch "$MANIFEST"

awk -F'\t' -v OFS='\t' '
  FILENAME == ARGV[1] { stored[$1] = $2 "\t" $3; next }
  stored[$1] != $2 "\t" $3 { print $1, $2 }' "$MANIFEST" "$TMP/remote.tsv" > "$TMP/todo.tsv"

PENDING=$OLAF_DIR/backup.pending
BATCH=25

# Old fingerprints are dropped only for tracks whose new audio converted, and
# right before storing: `olaf store` skips identifiers already indexed.
store_batch() {
  [ -s "$TMP/batch.tsv" ] || return 0
  touch "$PENDING"
  mapfile -t wavs < <(cut -f1 "$TMP/batch.tsv" | sed "s|.*|$CONVERTED/&.wav|")
  python3 "$SCRIPT_DIR/olaf_forget.py" "${wavs[@]}" || exit 1
  awk -F'\t' 'FILENAME == ARGV[1] { batch[$1]; next } !($1 in batch)' "$TMP/batch.tsv" "$MANIFEST" > "$TMP/manifest"
  cat "$TMP/manifest" > "$MANIFEST"
  while IFS=$'\t' read -r -u 3 id key etag; do
    if olaf store "$CONVERTED/$id.wav" > /dev/null; then
      printf '%s\t%s\t%s\n' "$id" "$key" "$etag" >> "$MANIFEST"
      log "Stored $key"
    else
      log "FAILED to store $key (will retry next run)"
    fi
    rm -f "$CONVERTED/$id.wav"
  done 3< "$TMP/batch.tsv"
  : > "$TMP/batch.tsv"
}

if [ -s "$TMP/todo.tsv" ]; then
  log "$(wc -l < "$TMP/todo.tsv") new or changed track(s)"
  : > "$TMP/batch.tsv"
  while IFS=$'\t' read -r -u 3 id key; do
    etag=$(awk -F'\t' -v id="$id" '$1 == id { print $3 }' "$TMP/remote.tsv")
    src="$WORK/${key#"$PREFIX"}"
    if "${S3CP[@]}" "s3://$BUCKET/$key" "$src" \
       && ffmpeg -nostdin -loglevel error -i "$src" -ar 16000 -ac 1 -y "$CONVERTED/$id.wav"; then
      printf '%s\t%s\t%s\n' "$id" "$key" "$etag" >> "$TMP/batch.tsv"
    else
      log "FAILED to fetch or convert $key (old fingerprints kept; will retry next run)"
      rm -f "$CONVERTED/$id.wav"
    fi
    rm -f "$src"
    [ "$(wc -l < "$TMP/batch.tsv")" -lt "$BATCH" ] || store_batch
  done 3< "$TMP/todo.tsv"
  store_batch
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

# Retried every run until an upload succeeds.
if [ -e "$PENDING" ]; then
  if tar -czf "$TMP/olaf_db.tar.gz" -C "$OLAF_DIR" ./db ./stored.txt ./stored.tsv ./collection.json \
     && "${S3CP[@]}" "$TMP/olaf_db.tar.gz" "s3://$BUCKET/$BACKUP_KEY"; then
    rm -f "$PENDING"
    log "Backed up Olaf DB"
  else
    log "Backup failed (will retry next run)"
  fi
fi
