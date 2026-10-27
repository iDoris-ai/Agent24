#!/usr/bin/env bash
# Documenting sixth release (r6) acceptance on a real machine, driven as a user
# drives it: `agent24 os install`, `agent24 daemon start`, `agent24 documents`;
# the daemon's HTTP API only where no product entry exists (T1, T2). Cases and
# the release scope: docs/documenting/acceptance/R6-RELEASE-USECASES.md.
#
#   scripts/doc1-r6-accept.sh --bin <bin dir> [--old <main bin dir>] [--work <dir>] [--next]
#
# The bin dir holds `agent24`, `agent24d` and `agent24-documents`
# (`cargo build -p agent24-cli -p agent24d -p agent24-documents`); --old the
# same three built from main (r5), for the upgrade cases M1-M7. --next adds
# the R cases, for CLI entries added after r6 (DOC-1-57): r6's binaries have
# none of them, so they fail there. Run it from
# the release checkout: the package is assembled from the domain-os.yml, the
# pdfkit engine sources and the samples found here. macOS only (the engine).
# The daemon runs on its own AGENT24_DATA_DIR under --work; Z1 checks the
# user's ~/.agent24, so stop your own daemon first. No model is needed.
# Needs jq, curl, perl, python3, sips (U9's PNG) and swiftc
# (engines/pdfkit/build.sh); with --old, sqlite3 too (M4 reads the
# migrations, which have no product entry).
set -uo pipefail
export LC_ALL=C
# Relative paths are the caller's: taken from where it ran, before the cd.
abs() { case "$1" in /*) printf '%s' "$1" ;; *) printf '%s/%s' "$PWD" "$1" ;; esac; }
BIN="" OLD="" WORK="" NEXT=""
while [ $# -gt 0 ]; do
  case "$1" in
    # Each takes a value: without one, `$(abs "$2")` fails in its subshell
    # only and `shift 2` shifts nothing, so the loop would never end.
    --bin|--old|--work) [ $# -ge 2 ] || { echo "$1 needs a value" >&2; exit 2; } ;;
  esac
  case "$1" in
    --bin) BIN=$(abs "$2"); shift 2 ;;
    --old) OLD=$(abs "$2"); shift 2 ;;
    --work) WORK=$(abs "$2"); shift 2 ;;
    --next) NEXT=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$BIN" ] || { sed -n 2,20p "$0" >&2; exit 2; }
cd "$(dirname "$0")/.."
REPO=$(pwd)
for b in agent24 agent24d agent24-documents; do
  [ -x "$BIN/$b" ] || { echo "missing binary: $BIN/$b" >&2; exit 2; }
  [ -z "$OLD" ] || [ -x "$OLD/$b" ] || { echo "missing binary: $OLD/$b" >&2; exit 2; }
done
for t in jq curl perl python3 sips ${OLD:+sqlite3}; do
  command -v "$t" >/dev/null || { echo "missing tool: $t" >&2; exit 2; }
done
resolve() { python3 -I -c 'import os, sys; print(os.path.realpath(sys.argv[1]))' "$1"; }
BIN=$(resolve "$BIN")
[ -z "$OLD" ] || OLD=$(resolve "$OLD")
REAL=$(resolve "${HOME:-/nonexistent}/.agent24")
WORK=$(resolve "${WORK:-${TMPDIR:-/tmp}/doc1-r6-accept.$$}")
case "$WORK/" in "$REAL"/*) echo "--work must not be inside $REAL" >&2; exit 2 ;; esac
[ -s "$REAL/daemon.json" ] && kill -0 "$(jq -r .pid "$REAL/daemon.json")" 2>/dev/null \
  && { echo "a daemon is running on $REAL: stop it first (agent24 daemon stop)" >&2; exit 2; }
[ -z "$(ls -A "$WORK" 2>/dev/null)" ] || { echo "--work must be new or empty: $WORK" >&2; exit 2; }
mkdir -p "$WORK" || exit 2
# Only what this script sets reaches the daemon or the CLI.
# AGENT24D_BIN too: the CLI would start that daemon instead of the one in --bin.
for v in $(env | grep -oE '^(A24|AGENT24)[A-Z0-9]*_[A-Z0-9_]+'); do unset "$v"; done
export AGENT24_DATA_DIR="$WORK/data"
# Z1: every entry under ~/.agent24 with its size and modification time.
# A snapshot that could not read everything is no evidence: it says so, and Z1 fails.
snapshot() {
  [ -e "$REAL" ] || return 0
  local out
  out=$(find "$REAL" -exec stat -f '%N %z %m' {} + 2>&1) || { echo "UNREADABLE: $out"; return 0; }
  printf '%s\n' "$out" | sort
}
snapshot > "$WORK/.real-before"

FAILS=0
: > "$WORK/results"
# check ID "what" ok|fail "evidence" ["known limitation"]: a failing check
# with a known limitation is KNOWN, not FAIL; once fixed it passes.
check() {
  local mark=PASS
  if [ "$3" != ok ]; then
    if [ -n "${5:-}" ]; then mark=KNOWN; else mark=FAIL; FAILS=$((FAILS + 1)); fi
  fi
  printf '| %s | %s | %s | %s%s |\n' "$1" "$2" "$mark" "$4" "${5:+ — known: $5}" >> "$WORK/results"
  echo "$mark $1 — $2 — $4"
}
is() { [ "$1" = "$2" ] && echo ok || echo fail; }
limit() { perl -e 'alarm shift; exec @ARGV' "$@"; }
# a24 ARGS...: the CLI with a time limit, so a hang is a failure, not a stall.
a24() { limit 120 "$BIN/agent24" "$@"; }
# last FILE JQ: JQ applied to the last line of FILE (the result line of --json).
last() { tail -n 1 "$1" | jq -r "$2" 2>/dev/null; }
one() { tr '\n' ' ' < "$1" | cut -c1-160; }
job_of() { grep -oE 'job_[0-9A-Z]{26}' "$1" | head -n 1; }
# settle JOB: the job's status once it leaves queued/running/cancelling.
settle() {
  local st=""
  for _ in $(seq 1 120); do
    st=$(a24 documents job "$1" --json 2>/dev/null | tail -n 1 | jq -r '.data.status // empty' 2>/dev/null)
    case $st in queued|running|cancelling|"") sleep 0.5 ;; *) break ;; esac
  done
  echo "$st"
}
# The daemon this run started is stopped however the run ends.
# C1 makes blobs/tmp read-only for a while: an interrupted run puts it back,
# or the work dir cannot be removed.
# Every data dir (E and M use their own), whichever is current.
cleanup() {
  [ -n "${TMPBLOBS:-}" ] && chmod u+w "$TMPBLOBS" 2>/dev/null
  local dir pid
  for dir in "$WORK/data" "$WORK/bare/data" "$WORK/up/data"; do
    [ -s "$dir/daemon.json" ] || continue
    AGENT24_DATA_DIR=$dir limit 30 "$BIN/agent24" daemon stop > /dev/null 2>&1 && continue
    pid=$(jq -r .pid "$dir/daemon.json" 2>/dev/null)
    [ -n "$pid" ] && [ "$pid" != null ] && kill -9 "$pid" 2>/dev/null
  done
  return 0
}
trap cleanup EXIT

SMALL=$REPO/docs/documenting/samples/s01/s01-02-en-epa-boil-water/source.pdf
BIG=$WORK/two-chunks.pdf
# Past the OS's 768 KiB chunk: one sample PDF, padded.
cat "$REPO/docs/documenting/samples/s01/s01-01-zh-sh-fee-2025/source.pdf" > "$BIG"
head -c 900000 /dev/zero >> "$BIG"
OTHER=$REPO/docs/documenting/samples/s01/s01-04-en-govuk-sa-deadlines/source.pdf

# --- P: install, start, ready -------------------------------------------------
PKG=$WORK/pkg; mkdir -p "$PKG/bin"
cp "$REPO/rust/apps/agent24-documents/domain-os.yml" "$PKG/"
cp "$BIN/agent24-documents" "$PKG/bin/"
"$REPO/rust/apps/agent24-documents/engines/pdfkit/build.sh" "$PKG/bin" > "$WORK/engine.log" 2>&1 \
  || { echo "the pdfkit engine did not build: $WORK/engine.log" >&2; exit 2; }

a24 os install "$PKG" > "$WORK/p1.out" 2>&1; rc=$?
check P1 "os install the documents package" "$(is "$rc/$(grep -c '^installed ' "$WORK/p1.out")" 0/1)" "rc=$rc; $(head -n 1 "$WORK/p1.out")"
PATH="$BIN:$PATH" limit 60 "$BIN/agent24" daemon start > "$WORK/p2.out" 2>&1; rc=$?
t0=$(perl -MTime::HiRes=time -e 'printf "%d", time * 1000')
check P2 "daemon start (resident, fresh data dir)" "$(is "$rc/$(grep -c '^daemon started (pid ' "$WORK/p2.out")" 0/1)" "rc=$rc; $(one "$WORK/p2.out")"
PORT=$(jq -r .port "$AGENT24_DATA_DIR/daemon.json" 2>/dev/null)
TOKEN=$(jq -r .token "$AGENT24_DATA_DIR/daemon.json" 2>/dev/null)
# A module finishes its handshake after `daemon start` returns; until then the
# OS answers module_not_ready (the import client's three tries are back to back).
ready=fail
for _ in $(seq 1 150); do
  a24 documents capabilities > "$WORK/p3.out" 2>&1 && { ready=ok; break; }
  sleep 0.2
done
t1=$(perl -MTime::HiRes=time -e 'printf "%d", time * 1000')
[ $((t1 - t0)) -le 30000 ] || ready=fail
check P3 "the documents module answers within 30 s of daemon start" "$ready" "after $((t1 - t0)) ms; storage $(jq -r .storage.state "$WORK/p3.out" 2>/dev/null)"

# --- U: import ------------------------------------------------------------------
a24 documents import "$SMALL" > "$WORK/u1.out" 2> "$WORK/u1.err"; rc=$?
JOB1=$(job_of "$WORK/u1.out")
check U1 "import a PDF: prints its job" "$(is "$rc/$(grep -cE '^import job job_[0-9A-Z]{26} \((queued|running|succeeded)\)$' "$WORK/u1.out")" 0/1)" "rc=$rc; $(one "$WORK/u1.out")"

a24 documents import "$SMALL" > "$WORK/u2.out" 2>/dev/null; rc=$?
check U2 "the same file again: the same job" "$(is "$rc/$(job_of "$WORK/u2.out")" "0/${JOB1:-no job from U1}")" "rc=$rc; $(one "$WORK/u2.out")"

# U1's import settles first, so its document is the older one in O3 and O4.
st1=$(settle "$JOB1")
a24 documents import --json "$BIG" > "$WORK/u3.out" 2>/dev/null; rc=$?
n=$(grep -c '"type":"progress"' "$WORK/u3.out")
# Progress comes before each chunk and once at the end: a second chunk shows
# as a line at the first chunk's end (786432 bytes).
second=$(grep -c '"sent":786432,' "$WORK/u3.out")
JOB3=$(last "$WORK/u3.out" '.data.job_id // empty')
[ "$n" -ge 3 ] && [ "$second" = 1 ] && ok=ok || ok=fail
[ "$ok" = ok ] && ok=$(is "$rc/$(last "$WORK/u3.out" '"\(.type)/\(.ok)"')" "0/result/true")
check U3 "--json, a file of two chunks: progress lines, then the result" "$ok" "rc=$rc; progress=$n, at 786432: $second; job=$JOB3"

a24 documents import "$WORK/secret-name-missing.pdf" > "$WORK/u4.out" 2> "$WORK/u4.err"; rc=$?
ok=$(is "$rc/$(cat "$WORK/u4.err")" "1/error: file_unreadable: the file could not be read")
grep -q secret-name "$WORK/u4.out" "$WORK/u4.err" && ok=fail
check U4 "a file that is not there: file_unreadable, exit 1, no path" "$ok" "rc=$rc; $(one "$WORK/u4.err")"

printf 'just text\n' > "$WORK/notes.txt"
a24 documents import --json "$WORK/notes.txt" > "$WORK/u5.out" 2>/dev/null; rc=$?
check U5 "a text file: unsupported_format, not retryable, exit 1" \
  "$(is "$rc/$(last "$WORK/u5.out" '"\(.status)/\(.error.code)/\(.error.details.retryable)"')" "1/422/unsupported_format/false")" \
  "rc=$rc; $(last "$WORK/u5.out" .error.message)"

printf '{"base":"http://127.0.0.1:%s","token":"%s","path":"%s"}' "$PORT" "$TOKEN" "$SMALL" \
  | limit 120 "$BIN/agent24" documents import --host-stdin --json > "$WORK/u6.out" 2>/dev/null; rc=$?
check U6 "--host-stdin --json (the desktop's way): the same job" "$(is "$rc/$(last "$WORK/u6.out" '.data.job_id')" "0/${JOB1:-no job from U1}")" "rc=$rc; $(tail -n 1 "$WORK/u6.out" | cut -c1-100)"

printf '{"base":"http://127.0.0.1:%s","token":"wrong","path":"%s"}' "$PORT" "$SMALL" \
  | limit 120 "$BIN/agent24" documents import --host-stdin > /dev/null 2> "$WORK/u7.err"; rc=$?
check U7 "a wrong token: unauthorized, exit 1" "$(is "$rc/$(grep -c '^error: unauthorized: ' "$WORK/u7.err")" 1/1)" "rc=$rc; $(one "$WORK/u7.err")"

# --- O: the other operations ----------------------------------------------------
a24 documents capabilities > "$WORK/o1.out" 2>&1; rc=$?
check O1 "capabilities: storage ready, the pdfkit engine there" \
  "$(is "$rc/$(jq -r '"\(.storage.state)/\([.engines[] | select(.id == "apple-pdfkit" and .kind == "parse") | .state] | first)"' "$WORK/o1.out" 2>/dev/null)" 0/ready/ready)" \
  "rc=$rc; $(jq -c '{os_version, storage, engines: [.engines[] | {id, kind, state}]}' "$WORK/o1.out" 2>/dev/null | cut -c1-140)"

st3=$(settle "$JOB3")
a24 documents job "$JOB1" > "$WORK/o2.out" 2>&1; rc=$?
check O2 "job: the import, one line, once it succeeds" "$(is "$rc/$st3/$(cat "$WORK/o2.out")" "0/succeeded/import job $JOB1 (succeeded)")" "rc=$rc; $(one "$WORK/o2.out"); two-chunk job $st3"
DOC1=$(a24 documents job "$JOB1" --json 2>/dev/null | tail -n 1 | jq -r '.data.document_id // empty')

a24 documents list --json > "$WORK/o3.out" 2>/dev/null; rc=$?
check O3 "list: both imported documents, newest first" \
  "$(is "$rc/$(last "$WORK/o3.out" '[.data.documents[].document_id] | length')/$(last "$WORK/o3.out" '.data.documents[-1].document_id')" "0/2/$DOC1")" \
  "rc=$rc; $(last "$WORK/o3.out" '[.data.documents[].document_id] | join(",")')"

a24 documents list --limit 1 --json > "$WORK/o4.out" 2>/dev/null; rc=$?
CUR=$(last "$WORK/o4.out" '.data.next_cursor // empty')
a24 documents list --limit 1 --cursor "$CUR" --json > "$WORK/o4b.out" 2>/dev/null; rc2=$?
check O4 "list --limit 1, then --cursor: one document a page, the second page the other" \
  "$(is "$rc/$rc2/$(last "$WORK/o4.out" '"\(.data.documents | length)/\(.data.documents[0].document_id != "'"$DOC1"'")"')/$(last "$WORK/o4b.out" '"\(.data.documents | length)/\(.data.documents[0].document_id)"')" "0/0/1/true/1/$DOC1")" \
  "rc=$rc/$rc2; cursor=${CUR:0:16}"

a24 documents get "$DOC1" > "$WORK/o5.out" 2>&1; rc=$?
check O5 "get: the document and its head revision, indented JSON" \
  "$(is "$rc/$(jq -r '"\(.document_id)/\(.head_revision)"' "$WORK/o5.out" 2>/dev/null)/$(grep -c '^  "document_id": ' "$WORK/o5.out")" "0/$DOC1/1/1")" \
  "rc=$rc; $(tr -d '\n ' < "$WORK/o5.out" | cut -c1-100)"

rc=1
for _ in $(seq 1 60); do
  a24 documents text "$DOC1" 1 --json > "$WORK/o6.out" 2>/dev/null && { rc=0; break; }
  sleep 1
done
words=$(last "$WORK/o6.out" '[.data.blocks[].text] | join(" ")' | grep -ciE 'boil')
check O6 "text <doc> 1: the PDF's own words (\"boil\")" "$(is "$rc/$([ "${words:-0}" -ge 1 ] && echo y)" 0/y)" \
  "rc=$rc; blocks=$(last "$WORK/o6.out" '.data.blocks | length'); $(last "$WORK/o6.out" '.data.blocks[0].text' | tr '\n' ' ' | cut -c1-60)"

a24 documents cancel "$JOB1" --json > "$WORK/o7.out" 2>/dev/null; rc=$?
check O7 "cancel a succeeded job: a no-op, the job as it was" "$(is "$rc/$(last "$WORK/o7.out" '"\(.data.status)/\(.data.attempt)"')" 0/succeeded/1)" "rc=$rc; $(last "$WORK/o7.out" .data.status)"

a24 documents retry "$JOB1" > /dev/null 2> "$WORK/o8.err"; rc=$?
check O8 "retry a succeeded job: refused, the job unchanged" \
  "$(is "$rc/$(cat "$WORK/o8.err")/$(settle "$JOB1")" "1/error: invalid_request: a succeeded job cannot be retried/succeeded")" "rc=$rc; $(one "$WORK/o8.err")"

a24 documents get ../../runs > /dev/null 2> "$WORK/o9.err"; rc=$?
check O9 "get with a malformed id: refused before sending" "$(is "$rc/$(cat "$WORK/o9.err")" "1/error: invalid_request: not an allowed documents operation")" "rc=$rc; $(one "$WORK/o9.err")"

a24 documents list --limit 0 --json > "$WORK/o10.out" 2>/dev/null; rc=$?
check O10 "list --limit 0 --json: a result line, invalid_request" "$(is "$rc/$(last "$WORK/o10.out" '"\(.type)/\(.error.code)"')" 1/result/invalid_request)" "rc=$rc; $(tail -n 1 "$WORK/o10.out" | cut -c1-100)"

printf '{"base":"http://127.0.0.1:%s","token":"%s","request":{"op":"get","documentId":"%s"}}' "$PORT" "$TOKEN" "$DOC1" \
  | limit 120 "$BIN/agent24" documents request --host-stdin --json > "$WORK/o11.out" 2>/dev/null; rc=$?
check O11 "request --host-stdin --json (the desktop's way): the same document" "$(is "$rc/$(last "$WORK/o11.out" '"\(.type)/\(.data.document_id)"')" "0/result/$DOC1")" "rc=$rc; $(tail -n 1 "$WORK/o11.out" | cut -c1-100)"

a24 documents request '{"op":"proxy","path":"/api/v1/runs"}' > /dev/null 2> "$WORK/o12.err"; rc=$?
check O12 "request with an operation that is not allowed: refused" "$(is "$rc/$(cat "$WORK/o12.err")" "1/error: invalid_request: not an allowed documents operation")" "rc=$rc; $(one "$WORK/o12.err")"

printf '{"base":"http://127.0.0.1:%s","token":"wrong","request":{"op":"list"}}' "$PORT" \
  | limit 120 "$BIN/agent24" documents request --host-stdin > /dev/null 2> "$WORK/o13.err"; rc=$?
check O13 "a wrong token: unauthorized, exit 1" "$(is "$rc/$(grep -c '^error: unauthorized: ' "$WORK/o13.err")" 1/1)" "rc=$rc; $(one "$WORK/o13.err")"

a24 documents job job_00000000000000000000000000 > /dev/null 2> "$WORK/o14.err"; rc=$?
check O14 "job that does not exist: not_found" "$(is "$rc/$(cat "$WORK/o14.err")" "1/error: not_found: no such job")" "rc=$rc; $(one "$WORK/o14.err")"

a24 documents get doc_00000000000000000000000000 > /dev/null 2> "$WORK/o15.err"; rc=$?
check O15 "get a document that does not exist: not_found" "$(is "$rc/$(cat "$WORK/o15.err")" "1/error: not_found: no such document")" "rc=$rc; $(one "$WORK/o15.err")"

# --- C: cancel and retry (#1186) --------------------------------------------------
# A blob store that cannot be written fails an import at its store stage:
# storage_unavailable, which the desktop offers to retry.
TMPBLOBS=$AGENT24_DATA_DIR/os/documents/blobs/tmp
chmod 500 "$TMPBLOBS"
a24 documents import "$OTHER" > "$WORK/c1.out" 2>/dev/null; JOBF=$(job_of "$WORK/c1.out")
cp "$OTHER" "$WORK/other-copy.pdf"; printf ' ' >> "$WORK/other-copy.pdf"
a24 documents import "$WORK/other-copy.pdf" > "$WORK/c1b.out" 2>/dev/null; JOBC=$(job_of "$WORK/c1b.out")
stf=$(settle "$JOBF"); stc=$(settle "$JOBC")
a24 documents job "$JOBF" --json > "$WORK/c1.json" 2>/dev/null
a24 documents job "$JOBC" --json > "$WORK/c1b.json" 2>/dev/null
chmod 700 "$TMPBLOBS"
check C1 "the blob store not writable: both imports fail, storage_unavailable" \
  "$(is "$stf/$stc/$(last "$WORK/c1.json" '"\(.data.error.code)/\(.data.attempt)"')/$(last "$WORK/c1b.json" '"\(.data.error.code)/\(.data.attempt)"')" failed/failed/storage_unavailable/1/storage_unavailable/1)" \
  "$JOBF $stf, $JOBC $stc; $(last "$WORK/c1.json" .data.error.message)"

a24 documents retry "$JOBF" > "$WORK/c2.out" 2>&1; rc=$?
st=$(settle "$JOBF")
a24 documents job "$JOBF" --json > "$WORK/c2.json" 2>/dev/null
check C2 "storage back: retry runs it again, attempt 2 succeeds" \
  "$(is "$rc/$st/$(last "$WORK/c2.json" '"\(.data.attempt)/\(.data.error)"')" 0/succeeded/2/null)" "rc=$rc; $(one "$WORK/c2.out"); $st"

a24 documents cancel "$JOBC" > "$WORK/c3.out" 2>&1; rc=$?
check C3 "cancel a failed job: cancelled" "$(is "$rc/$(settle "$JOBC")" 0/cancelled)" "rc=$rc; $(one "$WORK/c3.out")"

a24 documents retry "$JOBC" > "$WORK/c4.out" 2>&1; rc=$?
st=$(settle "$JOBC")
check C4 "retry a cancelled job: it runs again and succeeds" "$(is "$rc/$st" 0/succeeded)" "rc=$rc; $(one "$WORK/c4.out"); $st"

a24 documents list --json > "$WORK/c5.out" 2>/dev/null; rc=$?
retried=0
for j in "$JOBF" "$JOBC"; do
  d=$(a24 documents job "$j" --json 2>/dev/null | tail -n 1 | jq -r '.data.document_id // empty')
  [ -n "$d" ] && last "$WORK/c5.out" '.data.documents[].document_id' | grep -qxF "$d" && retried=$((retried + 1))
done
check C5 "list: the two retried imports are documents now" "$(is "$rc/$(last "$WORK/c5.out" '.data.documents | length')/$retried" 0/4/2)" \
  "rc=$rc; documents=$(last "$WORK/c5.out" '.data.documents | length'), retried ones listed: $retried"

# --- U, again: images and file names (after C5, which counts the documents) ------
doc_of() { a24 documents job "$1" --json 2>/dev/null | tail -n 1 | jq -r '.data.document_id // empty'; }
# readtext DOC OUT: page 1's text. An image's text layer is built (OCR) on its
# first read, which answers engine_unavailable until it is there; any other
# answer is final. No new read starts after 60 s: each read itself may wait
# 8 s for the layer before it answers, so the bound is the clock, not a
# count, and the last read may end that much later. The case judges the
# words, not the time.
readtext() {
  local t0
  t0=$(perl -MTime::HiRes=time -e 'printf "%d", time * 1000')
  # The deadline is checked right before each read, after its pause.
  while [ $(($(perl -MTime::HiRes=time -e 'printf "%d", time * 1000') - t0)) -lt 60000 ]; do
    a24 documents text "$1" 1 --json > "$2" 2>/dev/null && return 0
    [ "$(last "$2" .error.code)" = engine_unavailable ] || return 1
    sleep 1
  done
  return 1
}
# image ID WHAT FILE MEDIA: import a scan of the 2026 holiday notice, read its
# words back. Its page has no text layer: "春节" can only come from the OCR.
image() {
  local rc st doc mt words
  a24 documents import "$3" > "$WORK/$1.out" 2>&1; rc=$?
  st=$(settle "$(job_of "$WORK/$1.out")"); doc=$(doc_of "$(job_of "$WORK/$1.out")")
  mt=$(a24 documents get "$doc" 2>/dev/null | jq -r .head.media_type 2>/dev/null)
  readtext "$doc" "$WORK/$1.text"
  words=$(last "$WORK/$1.text" '[.data.blocks[].text] | join(" ") | test("春节")')
  check "$1" "$2" "$(is "$rc/$st/$mt/$words" "0/succeeded/$4/true")" \
    "rc=$rc; $st; $mt; blocks=$(last "$WORK/$1.text" '.data.blocks | length'), 春节: $words$(last "$WORK/$1.text" '.error.code // empty' | sed 's/^./; &/')"
}
SCAN=$REPO/docs/documenting/samples/s01/s01-06-zh-holiday-2026-scan/source.jpg
image U8 "import a JPG scan: its words read back through OCR" "$SCAN" image/jpeg
sips -s format png "$SCAN" --out "$WORK/scan.png" > /dev/null 2>&1
image U9 "import a PNG of the same scan: its words read back" "$WORK/scan.png" image/png

# U1's PDF under another name: the name is part of the import's key, so this
# is an import of its own, not U1's job again.
NAMED="$WORK/煮沸 通知.pdf"
cp "$SMALL" "$NAMED"
a24 documents import "$NAMED" > "$WORK/u10.out" 2>&1; rc=$?
JOBN=$(job_of "$WORK/u10.out"); st=$(settle "$JOBN")
title=$(a24 documents get "$(doc_of "$JOBN")" 2>/dev/null | jq -r .title 2>/dev/null)
check U10 "a Chinese file name with a space: the document keeps it as its title" "$(is "$rc/$st/$title" "0/succeeded/煮沸 通知.pdf")" "rc=$rc; $st; title=$title"

# --- T: the agent-facing tools (not in this release) -----------------------------
code=$(curl -s -m 30 -o "$WORK/t1.out" -w '%{http_code}' -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:$PORT/api/v1/tools")
listed=$(jq -r 'if (.tools | type) == "array" and (.tools | length) > 0 then "listed" else "no list" end' "$WORK/t1.out" 2>/dev/null)
dt=$(jq -r '[.tools[] | (.name // "") | select(test("^documents"))] | length' "$WORK/t1.out" 2>/dev/null)
# KNOWN only for the gap itself: a list that came back without them.
if [ "$code/$listed" != 200/listed ]; then
  check T1 "the agent is offered the documents tools" fail "the tool list itself failed: $code $listed; $(cut -c1-80 "$WORK/t1.out")"
else
  check T1 "the agent is offered the documents tools" "$([ "${dt:-0}" -ge 1 ] && echo ok || echo fail)" \
    "tools=$(jq -r '.tools | length' "$WORK/t1.out"), documents=$dt" \
    "domain-os.yml declares no tools: (#705); the tools exist in the module but no chat can call them"
fi
code=$(curl -s -m 30 -o "$WORK/t2.out" -w '%{http_code}' -X POST -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d '{"context":{},"arguments":{}}' \
  "http://127.0.0.1:$PORT/api/v1/documents/_a24/tools/find")
check T2 "the module's tool route is not reachable from outside" "$(is "$code/$(jq -r .error.code "$WORK/t2.out" 2>/dev/null)" 404/not_found)" "$code $(cut -c1-80 "$WORK/t2.out")"

# --- R: reading, finding, extracting, rendering (after r6, only with --next) --------
# DOC-1-57: `text --block/--limit/--require-complete`, `find`, `extract`,
# `extraction`, `render`. On U1's document (DOC1), whose page 1 O6 read whole:
# what a page holds is judged against that read, not against fixed ids.
if [ -n "$NEXT" ]; then
  ids() { last "$1" '[.data.blocks[].block_id] | join(",")'; }
  # lines OUT: how many lines a --json run printed (its result is the only
  # one), a last line with no newline counted too.
  lines() { awk 'END { print NR }' "$1"; }
  # dims PNG: its pixel size as WxH ("" for a file that is no image).
  dims() { sips -g pixelWidth -g pixelHeight "$1" 2>/dev/null | awk '/pixel/ { print $2 }' | paste -sd x -; }
  png() { [ "$(head -c 8 "$1" 2>/dev/null | od -An -tx1 | tr -d ' \n')" = 89504e470d0a1a0a ] && echo y; }
  full=$(last "$WORK/o6.out" '[.data.blocks[:4][].block_id] | join(",")')

  a24 documents text "$DOC1" 1 --limit 2 --json > "$WORK/r1.out" 2>/dev/null; rc=$?
  CUR=$(last "$WORK/r1.out" '.data.next_cursor // empty')
  a24 documents text "$DOC1" 1 --limit 2 --cursor "$CUR" --json > "$WORK/r1b.out" 2>/dev/null; rc2=$?
  check R1 "text --limit 2, then --cursor: two blocks a page, the next two after them" \
    "$(is "$rc/$rc2/$(last "$WORK/r1.out" '.data.blocks | length')/$(last "$WORK/r1b.out" '.data.blocks | length')/$(ids "$WORK/r1.out"),$(ids "$WORK/r1b.out")" "0/0/2/2/$full")" \
    "rc=$rc/$rc2; $(ids "$WORK/r1.out") | $(ids "$WORK/r1b.out"); whole read: $full"

  third=$(last "$WORK/o6.out" '.data.blocks[2].block_id')
  a24 documents text "$DOC1" 1 --block "$third" --limit 1 --json > "$WORK/r2.out" 2>/dev/null; rc=$?
  check R2 "text --block: starts at that block" \
    "$(is "$rc/$(ids "$WORK/r2.out")/$(last "$WORK/r2.out" '.data.blocks[0].text')" "0/$third/$(last "$WORK/o6.out" '.data.blocks[2].text')")" \
    "rc=$rc; ${third:-no block 3 in O6}: $(ids "$WORK/r2.out")"

  a24 documents text "$DOC1" 1 --block "$third" --cursor "$CUR" --json > "$WORK/r3.out" 2>/dev/null; rc=$?
  check R3 "text --block with --cursor: a result line, invalid_request, exit 1" \
    "$(is "$rc/$(lines "$WORK/r3.out")/$(last "$WORK/r3.out" '"\(.type)/\(.status)/\(.error.code)/\(.error.message | test("give block or cursor, not both"))"')" 1/1/result/400/invalid_request/true)" \
    "rc=$rc; $(last "$WORK/r3.out" .error.message)"

  a24 documents find "$DOC1" 1 boil --json > "$WORK/r4.out" 2>/dev/null; rc=$?
  n=$(last "$WORK/r4.out" '.data.matches | length')
  each=$(last "$WORK/r4.out" '[.data.matches[] | .document_id == "'"$DOC1"'" and .page == 1 and (.quote | test("boil"; "i")) and (.block_id | type) == "string" and (.text_range.start | type) == "number"] | all')
  check R4 "find: every match is an anchor on page 1 quoting the word" \
    "$(is "$rc/$([ "${n:-0}" -ge 2 ] && echo y)/$each" 0/y/true)" "rc=$rc; matches=$n; first $(last "$WORK/r4.out" '.data.matches[0] | "\(.block_id) \(.quote)"')"

  # at OUT I: match I's place as block@start, or nothing when there is no such
  # match: a missing place never compares equal to another missing one.
  at() { last "$1" ".data.matches[$2] | select((.block_id | type) == \"string\" and (.text_range.start | type) == \"number\") | \"\(.block_id)@\(.text_range.start)\""; }
  a24 documents find "$DOC1" 1 boil --limit 1 --json > "$WORK/r5.out" 2>/dev/null; rc=$?
  FCUR=$(last "$WORK/r5.out" '.data.next_cursor // empty')
  a24 documents find "$DOC1" 1 boil --limit 1 --cursor "$FCUR" --json > "$WORK/r5b.out" 2>/dev/null; rc2=$?
  check R5 "find --limit 1, then --cursor: the first match, then the second" \
    "$(is "$rc/$rc2/$(last "$WORK/r5.out" '.data.matches | length')/$(last "$WORK/r5b.out" '.data.matches | length')/$(at "$WORK/r5.out" 0)/$(at "$WORK/r5b.out" 0)/$([ -n "$(at "$WORK/r4.out" 0)" ] && [ -n "$(at "$WORK/r4.out" 1)" ] && echo both)" "0/0/1/1/$(at "$WORK/r4.out" 0)/$(at "$WORK/r4.out" 1)/both")" \
    "rc=$rc/$rc2; $(at "$WORK/r5.out" 0) then $(at "$WORK/r5b.out" 0)"

  a24 documents find "$DOC1" 1 zzqqxxjj --json > "$WORK/r6.out" 2>/dev/null; rc=$?
  check R6 "find what is not there: no matches, no cursor" \
    "$(is "$rc/$(last "$WORK/r6.out" '"\(.data.matches | type)/\(.data.matches | length)/\(.data | has("next_cursor"))/\(.data.next_cursor)"')" 0/array/0/true/null)" "rc=$rc"

  a24 documents render "$DOC1" 1 1 --out "$WORK/r7.png" > "$WORK/r7.out" 2>&1; rc=$?
  check R7 "render --out: the page as a PNG at scale 1, the file named in the output" \
    "$(is "$rc/$(png "$WORK/r7.png")/$(dims "$WORK/r7.png")/$(jq -r '"\(.scale == 1)/\(.written_to)"' "$WORK/r7.out" 2>/dev/null)" "0/y/612x792/true/$WORK/r7.png")" \
    "rc=$rc; $(dims "$WORK/r7.png"); $(tr -d '\n ' < "$WORK/r7.out" | cut -c1-100)"

  a24 documents render "$DOC1" 1 1 --scale 0.5 --region 0,0,100,50 --json > "$WORK/r8.out" 2>/dev/null; rc=$?
  last "$WORK/r8.out" '.data.png // empty' | base64 -d > "$WORK/r8.png" 2>/dev/null
  check R8 "render --scale 0.5 --region: that area, at that scale, in base64 with --json" \
    "$(is "$rc/$(lines "$WORK/r8.out")/$(last "$WORK/r8.out" '"\(.data.media_type)/\(.data.scale == 0.5)/\(.data.bytes)"')/$(png "$WORK/r8.png")/$(dims "$WORK/r8.png")" "0/1/image/png/true/$(wc -c < "$WORK/r8.png" | tr -d ' ')/y/50x25")" \
    "rc=$rc; $(dims "$WORK/r8.png"), $(wc -c < "$WORK/r8.png" | tr -d ' ') bytes"

  a24 documents render "$DOC1" 1 99 --json > "$WORK/r9.out" 2>/dev/null; rc=$?
  check R9 "render a page that is not there: not_found, exit 1" "$(is "$rc/$(last "$WORK/r9.out" '"\(.status)/\(.error.code)"')" 1/404/not_found)" "rc=$rc; $(last "$WORK/r9.out" .error.message)"

  a24 documents render "$DOC1" 1 1 --out "$WORK/no/such/dir/p.png" --json > "$WORK/r10.out" 2>/dev/null; rc=$?
  check R10 "render --out where no file can be made: file_unwritable, exit 1, nothing written" \
    "$(is "$rc/$(last "$WORK/r10.out" '"\(.status)/\(.error.code)"')/$([ -e "$WORK/no" ] && echo made)" 1/400/file_unwritable/)" "rc=$rc"

  printf '{"fields":[{"key":"issued_by","description":"who issued the notice","type":"text"}]}' > "$WORK/ask.json"
  # No model is needed: what is judged is the job made and its end once
  # cancelled, not what the model would extract.
  a24 documents extract "$DOC1" 1 --schema "$WORK/ask.json" > "$WORK/r11.out" 2>&1; rc=$?
  JOBX=$(job_of "$WORK/r11.out")
  kind=$(a24 documents job "$JOBX" --json 2>/dev/null | tail -n 1 | jq -r .data.kind 2>/dev/null)
  a24 documents cancel "$JOBX" > /dev/null 2>&1; rc2=$?
  check R11 "extract: an extract job, which cancel then ends" \
    "$(is "$rc/$(grep -cE '^extract job job_[0-9A-Z]{26} \((queued|running)\)$' "$WORK/r11.out")/$kind/$rc2/$(settle "$JOBX")" 0/1/extract/0/cancelled)" \
    "rc=$rc/$rc2; $(one "$WORK/r11.out")"

  a24 documents extract "$DOC1" 1 --schema "$WORK/ask.json" --rerun --json > "$WORK/r12.out" 2>/dev/null; rc=$?
  check R12 "extract --rerun without --key: a result line, the key is required, exit 1" \
    "$(is "$rc/$(lines "$WORK/r12.out")/$(last "$WORK/r12.out" '"\(.type)/\(.status)/\(.error.message)"')" "1/1/result/400/the Idempotency-Key header is required")" "rc=$rc"

  a24 documents extract "$DOC1" 1 --schema "$WORK/ask.json" --key k1 --json > "$WORK/r13.out" 2>/dev/null; rc=$?
  check R13 "extract --key without --rerun: refused, not ignored" \
    "$(is "$rc/$(lines "$WORK/r13.out")/$(last "$WORK/r13.out" '"\(.status)/\(.error.code)/\(.error.message)"')" "1/1/400/invalid_request/--key names a rerun; give --rerun with it")" "rc=$rc"

  printf 'fields: issued_by\n' > "$WORK/ask.txt"
  a24 documents extract "$DOC1" 1 --schema "$WORK/ask.txt" --json > "$WORK/r14.out" 2>/dev/null; rc=$?
  check R14 "extract with a schema that is not JSON: refused before sending" \
    "$(is "$rc/$(lines "$WORK/r14.out")/$(last "$WORK/r14.out" '"\(.status)/\(.error.message)"')" "1/1/400/the schema is not JSON")" "rc=$rc"

  a24 documents extraction ext_00000000000000000000000000 --json > "$WORK/r15.out" 2>/dev/null; rc=$?
  check R15 "extraction that does not exist: not_found" "$(is "$rc/$(lines "$WORK/r15.out")/$(last "$WORK/r15.out" '"\(.status)/\(.error.message)"')" "1/1/404/no such extraction")" "rc=$rc"

  printf '{"base":"http://127.0.0.1:%s","token":"%s","request":{"op":"find","documentId":"%s","revision":1,"query":"boil"}}' "$PORT" "$TOKEN" "$DOC1" \
    | limit 120 "$BIN/agent24" documents request --host-stdin --json > "$WORK/r16.out" 2>/dev/null; rc=$?
  check R16 "find through request --host-stdin (the desktop's way): the same matches" \
    "$(is "$rc/$(lines "$WORK/r16.out")/$(last "$WORK/r16.out" '"\(.type)/\(.data.matches | type)/\(.data.matches | length)"')/$([ "${n:-0}" -ge 2 ] && echo ref)" "0/1/result/array/$n/ref")" "rc=$rc; R4 had $n"

  printf '{"base":"http://127.0.0.1:%s","token":"%s","request":{"op":"renderPage","documentId":"%s","revision":1,"page":1,"scale":0.25}}' "$PORT" "$TOKEN" "$DOC1" \
    | limit 120 "$BIN/agent24" documents request --host-stdin --json > "$WORK/r17.out" 2>/dev/null; rc=$?
  last "$WORK/r17.out" '.data.png // empty' | base64 -d > "$WORK/r17.png" 2>/dev/null
  check R17 "renderPage through request --host-stdin: the PNG in the result line" \
    "$(is "$rc/$(lines "$WORK/r17.out")/$(last "$WORK/r17.out" '"\(.type)/\(.data.media_type)/\(.data.scale == 0.25)"')/$(png "$WORK/r17.png")/$(dims "$WORK/r17.png")" "0/1/result/image/png/true/y/153x198")" \
    "rc=$rc; $(dims "$WORK/r17.png")"
fi

# --- N: no daemon -------------------------------------------------------------------
limit 60 "$BIN/agent24" daemon stop > "$WORK/n0.out" 2>&1
for _ in $(seq 1 40); do [ -s "$AGENT24_DATA_DIR/daemon.json" ] || break; sleep 0.5; done
a24 documents list > /dev/null 2> "$WORK/n1.err"; rc=$?
ok=$(is "$rc/$(grep -c 'no running daemon: start one with `agent24 daemon start`' "$WORK/n1.err")" 1/1)
[ -s "$AGENT24_DATA_DIR/daemon.json" ] && ok=fail
check N1 "no daemon running: says so, starts none" "$ok" "rc=$rc; $(one "$WORK/n1.err")"

a24 documents job "$JOB1" --json > "$WORK/n2.out" 2>/dev/null; rc=$?
check N2 "job --json with no daemon: one result line, backend_not_ready" "$(is "$rc/$(wc -l < "$WORK/n2.out" | tr -d ' ')/$(last "$WORK/n2.out" '"\(.type)/\(.error.code)"')" 1/1/result/backend_not_ready)" "rc=$rc; $(cut -c1-100 "$WORK/n2.out")"

a24 documents import --json "$SMALL" > "$WORK/n3.out" 2>/dev/null; rc=$?
check N3 "import --json with no daemon: one result line, backend_not_ready" "$(is "$rc/$(wc -l < "$WORK/n3.out" | tr -d ' ')/$(last "$WORK/n3.out" '"\(.type)/\(.error.code)"')" 1/1/result/backend_not_ready)" "rc=$rc; $(cut -c1-100 "$WORK/n3.out")"

a24 documents list --limit 0 --json > "$WORK/n4.out" 2>/dev/null; rc=$?
check N4 "a malformed request with no daemon: invalid_request, not backend_not_ready" "$(is "$rc/$(last "$WORK/n4.out" .error.code)" 1/invalid_request)" "rc=$rc; $(cut -c1-100 "$WORK/n4.out")"

printf 'not json' | limit 60 "$BIN/agent24" documents import --host-stdin --json > "$WORK/n5.out" 2>/dev/null; rc=$?
check N5 "--host-stdin --json with bad input: a result line, invalid_request" "$(is "$rc/$(last "$WORK/n5.out" '"\(.type)/\(.error.code)"')" 1/result/invalid_request)" "rc=$rc; $(cut -c1-100 "$WORK/n5.out")"

# --- E: a package without the read engine ---------------------------------------------
# What a user gets who skipped engines/pdfkit/build.sh (the module README's
# step), or runs where the engine cannot be: imports work, reading does not.
export AGENT24_DATA_DIR="$WORK/bare/data"
BARE=$WORK/bare/pkg; mkdir -p "$BARE/bin"
cp "$REPO/rust/apps/agent24-documents/domain-os.yml" "$BARE/"
cp "$BIN/agent24-documents" "$BARE/bin/"
a24 os install "$BARE" > "$WORK/e1.out" 2>&1; r1=$?
PATH="$BIN:$PATH" limit 60 "$BIN/agent24" daemon start >> "$WORK/e1.out" 2>&1; r2=$?
r3=1
for _ in $(seq 1 150); do
  a24 documents capabilities > "$WORK/e1.json" 2>/dev/null && { r3=0; break; }
  sleep 0.2
done
check E1 "no engine in the package: the module starts, both pdfkit engines absent" \
  "$(is "$r1/$r2/$r3/$(jq -r '"\(.storage.state)/" + ([.engines[] | select(.id == "apple-pdfkit") | "\(.kind):\(.state)"] | sort | join(","))' "$WORK/e1.json" 2>/dev/null)" "0/0/0/ready/parse:absent,render:absent")" \
  "rc=$r1/$r2/$r3; $(jq -c '[.engines[] | {id, kind, state}]' "$WORK/e1.json" 2>/dev/null | cut -c1-120)"

a24 documents import "$SMALL" > "$WORK/e2.out" 2>&1; rc=$?
JOBE=$(job_of "$WORK/e2.out"); st=$(settle "$JOBE")
check E2 "no engine: an import still succeeds" "$(is "$rc/$st" 0/succeeded)" "rc=$rc; $JOBE $st"

a24 documents text "$(doc_of "$JOBE")" 1 --json > "$WORK/e3.out" 2>/dev/null; rc=$?
check E3 "no engine: text answers engine_unavailable (503, retryable), exit 1" \
  "$(is "$rc/$(last "$WORK/e3.out" '"\(.type)/\(.status)/\(.error.code)/\(.error.details.engine)/\(.error.details.retryable)/\(.error.message)"')" \
    "1/result/503/engine_unavailable/apple-pdfkit/true/no read engine on this platform")" \
  "rc=$rc; $(tail -n 1 "$WORK/e3.out" | cut -c1-140)"
limit 60 "$BIN/agent24" daemon stop > /dev/null 2>&1
for _ in $(seq 1 40); do [ -s "$AGENT24_DATA_DIR/daemon.json" ] || break; sleep 0.5; done

# --- M: upgrading from r5 (main), only with --old ------------------------------------
# r5's CLI has no `documents`: its side is driven with its own `os` and
# `daemon`, and documents through this release's CLI, a REST client of the API
# r5 already served (the r5 desktop's calls).
if [ -n "$OLD" ]; then
  export AGENT24_DATA_DIR="$WORK/up/data"
  DB=$AGENT24_DATA_DIR/os/documents/documents.db
  OLDPKG=$WORK/oldpkg; mkdir -p "$OLDPKG/bin"
  cp "$REPO/rust/apps/agent24-documents/domain-os.yml" "$OLDPKG/"
  cp "$OLD/agent24-documents" "$PKG/bin/agent24-documents-pdfkit" "$OLDPKG/bin/"
  # ready: the documents module answers within 30 s of now (as P3 judges it).
  ready() {
    local t0 t1
    t0=$(perl -MTime::HiRes=time -e 'printf "%d", time * 1000')
    for _ in $(seq 1 150); do
      a24 documents capabilities > /dev/null 2>&1 || { sleep 0.2; continue; }
      t1=$(perl -MTime::HiRes=time -e 'printf "%d", time * 1000')
      [ $((t1 - t0)) -le 30000 ]; return
    done
    return 1
  }
  dbv() { sqlite3 "$DB" 'SELECT max(version) || "/" || min(success) FROM _sqlx_migrations' 2>/dev/null; }
  # A library sqlite3 could not read is no evidence: it hashes to a marker, never
  # to the hash of nothing, so two failed reads cannot compare equal as "unchanged".
  dump() {
    local out
    out=$(sqlite3 "$DB" .dump 2>/dev/null) && [ -n "$out" ] || { echo "unreadable-$RANDOM$RANDOM"; return; }
    printf '%s' "$out" | shasum | cut -c1-16
  }

  limit 120 "$OLD/agent24" os install "$OLDPKG" > "$WORK/m1.out" 2>&1; r1=$?
  PATH="$OLD:$PATH" limit 60 "$OLD/agent24" daemon start >> "$WORK/m1.out" 2>&1; r2=$?
  ready; r3=$?
  a24 documents import "$SMALL" > "$WORK/m1i.out" 2>/dev/null; JOBM=$(job_of "$WORK/m1i.out")
  stm=$(settle "$JOBM")
  DOCM=$(a24 documents job "$JOBM" --json 2>/dev/null | tail -n 1 | jq -r '.data.document_id // empty')
  a24 documents text "$DOCM" 1 --json > "$WORK/m1t.out" 2>/dev/null
  blocks=$(last "$WORK/m1t.out" '.data.blocks | length')
  limit 60 "$OLD/agent24" daemon stop >> "$WORK/m1.out" 2>&1
  for _ in $(seq 1 40); do [ -s "$AGENT24_DATA_DIR/daemon.json" ] || break; sleep 0.5; done
  v5=$(dbv)
  check M1 "r5 builds a library: install, start, import a PDF, read its text" \
    "$(is "$r1/$r2/$r3/$stm/$([ "${blocks:-0}" -ge 1 ] && echo y)/${v5%%/*}" "0/0/0/succeeded/y/6")" \
    "r5 migrations $v5; $DOCM, $blocks blocks"

  # The upgrade as a user does it: `os install` refuses a name already there.
  a24 os uninstall documents > "$WORK/m2.out" 2>&1; r1=$?
  a24 os install "$PKG" >> "$WORK/m2.out" 2>&1; r2=$?
  PATH="$BIN:$PATH" limit 60 "$BIN/agent24" daemon start >> "$WORK/m2.out" 2>&1; r3=$?
  ready; r4=$?
  check M2 "upgrade: os uninstall, os install the r6 package, daemon start" "$(is "$r1/$r2/$r3/$r4" 0/0/0/0)" \
    "rc=$r1/$r2/$r3/$r4; $(grep -E '^(removed|installed|daemon started)' "$WORK/m2.out" | cut -c1-40 | tr '\n' ' ')"

  st=$(settle "$JOBM")
  a24 documents text "$DOCM" 1 --json > "$WORK/m3t.out" 2>/dev/null; rc=$?
  check M3 "r5's document, job and text are all there" \
    "$(is "$st/$rc/$(last "$WORK/m3t.out" '.data.blocks | length')/$(a24 documents get "$DOCM" 2>/dev/null | jq -r .head_revision)" "succeeded/0/$blocks/1")" \
    "job $st; $(last "$WORK/m3t.out" '.data.blocks | length') blocks"

  v6=$(dbv)
  check M4 "the documents library migrated to 0008, every step a success" "$(is "$v6" 8/1)" "r5 $v5 → r6 $v6"

  a24 documents import "$OTHER" > "$WORK/m5.out" 2>/dev/null; st=$(settle "$(job_of "$WORK/m5.out")")
  a24 documents list --json > "$WORK/m5l.out" 2>/dev/null
  check M5 "on the upgraded library: a new import, then both are listed" \
    "$(is "$st/$(last "$WORK/m5l.out" '.data.documents | length')" succeeded/2)" "import $st; $(last "$WORK/m5l.out" '.data.documents | length') documents"

  # Rolling back: r5 cannot read a library r6 migrated, and must not write it.
  limit 60 "$BIN/agent24" daemon stop > "$WORK/m6.out" 2>&1
  for _ in $(seq 1 40); do [ -s "$AGENT24_DATA_DIR/daemon.json" ] || break; sleep 0.5; done
  h1=$(dump); vb=$(dbv)
  a24 os uninstall documents >> "$WORK/m6.out" 2>&1
  limit 120 "$OLD/agent24" os install "$OLDPKG" >> "$WORK/m6.out" 2>&1
  PATH="$OLD:$PATH" limit 60 "$OLD/agent24" daemon start >> "$WORK/m6.out" 2>&1; r1=$?
  st=""
  for _ in $(seq 1 150); do
    a24 documents list --json > "$WORK/m6l.out" 2>/dev/null; rl=$?
    st=$(last "$WORK/m6l.out" '.error.code // "listed"')
    [ "$st" = module_not_ready ] || break
    sleep 0.2
  done
  limit 60 "$OLD/agent24" daemon stop >> "$WORK/m6.out" 2>&1
  for _ in $(seq 1 40); do [ -s "$AGENT24_DATA_DIR/daemon.json" ] || break; sleep 0.5; done
  h2=$(dump)
  # r5's kernel must start for this to say anything about the documents library:
  # a kernel store migration newer than r5's stops it first (its error is shown).
  check M6 "back to r5: the library refused (storage_unavailable), not changed" \
    "$(is "$vb/$r1/$rl/$st/$h2" "8/1/0/1/storage_unavailable/$h1")" \
    "library $vb; rc=$r1, list rc=$rl; $(last "$WORK/m6l.out" .error.message); dump $h1 → $h2$(grep -m 1 '^error: ' "$WORK/m6.out" | cut -c1-140 | sed 's/^/; r5: /')"

  a24 os uninstall documents > "$WORK/m7.out" 2>&1
  a24 os install "$PKG" >> "$WORK/m7.out" 2>&1
  PATH="$BIN:$PATH" limit 60 "$BIN/agent24" daemon start >> "$WORK/m7.out" 2>&1; r1=$?
  ready; r2=$?
  a24 documents list --json > "$WORK/m7l.out" 2>/dev/null
  check M7 "r6 again: the library as it was, both documents" "$(is "$r1/$r2/$(last "$WORK/m7l.out" '.data.documents | length')" 0/0/2)" \
    "rc=$r1/$r2; $(last "$WORK/m7l.out" '.data.documents | length') documents"
  limit 60 "$BIN/agent24" daemon stop > /dev/null 2>&1
fi

snapshot > "$WORK/.real-after"
z1=$(cmp -s "$WORK/.real-before" "$WORK/.real-after" && echo ok || echo fail)
grep -q '^UNREADABLE: ' "$WORK/.real-before" "$WORK/.real-after" && z1=fail
check Z1 "the user's ~/.agent24 was not touched" "$z1" \
  "entries before=$(wc -l < "$WORK/.real-before" | tr -d ' ') after=$(wc -l < "$WORK/.real-after" | tr -d ' '); $(diff "$WORK/.real-before" "$WORK/.real-after" | grep -c '^[<>]') changed"

{
  echo "| ID | case | result | evidence |"
  echo "|---|---|---|---|"
  cat "$WORK/results"
  echo
  echo "$(grep -c '| PASS |' "$WORK/results") passed, $(grep -c '| KNOWN |' "$WORK/results") known, $FAILS failed."
} > "$WORK/report.md"
echo
tail -n 1 "$WORK/report.md"
echo "report: $WORK/report.md"
[ "$FAILS" = 0 ]
