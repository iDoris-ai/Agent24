#!/usr/bin/env bash
# Documenting sixth release (r6) acceptance on a real machine, driven as a user
# drives it: `agent24 os install`, `agent24 daemon start`, `agent24 documents`;
# the daemon's HTTP API only where no product entry exists (T1, T2). Cases and
# the release scope: docs/documenting/acceptance/R6-RELEASE-USECASES.md.
#
#   scripts/doc1-r6-accept.sh --bin <bin dir> [--work <dir>]
#
# The bin dir holds `agent24`, `agent24d` and `agent24-documents`
# (`cargo build -p agent24-cli -p agent24d -p agent24-documents`). Run it from
# the release checkout: the package is assembled from the domain-os.yml, the
# pdfkit engine sources and the samples found here. macOS only (the engine).
# The daemon runs on its own AGENT24_DATA_DIR under --work; Z1 checks the
# user's ~/.agent24, so stop your own daemon first. No model is needed.
# Needs jq, curl, perl, python3 and swiftc (engines/pdfkit/build.sh).
set -uo pipefail
export LC_ALL=C
cd "$(dirname "$0")/.."
REPO=$(pwd)

BIN="" WORK=""
while [ $# -gt 0 ]; do
  case "$1" in
    --bin) BIN=$2; shift 2 ;;
    --work) WORK=$2; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$BIN" ] || { sed -n 2,14p "$0" >&2; exit 2; }
for b in agent24 agent24d agent24-documents; do
  [ -x "$BIN/$b" ] || { echo "missing binary: $BIN/$b" >&2; exit 2; }
done
for t in jq curl perl python3; do
  command -v "$t" >/dev/null || { echo "missing tool: $t" >&2; exit 2; }
done
resolve() { python3 -I -c 'import os, sys; print(os.path.realpath(sys.argv[1]))' "$1"; }
BIN=$(resolve "$BIN")
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
snapshot() { [ -e "$REAL" ] && find "$REAL" -exec stat -f '%N %z %m' {} + 2>/dev/null | sort; }
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
cleanup() {
  [ -s "$AGENT24_DATA_DIR/daemon.json" ] || return 0
  limit 30 "$BIN/agent24" daemon stop > /dev/null 2>&1 && return 0
  local pid; pid=$(jq -r .pid "$AGENT24_DATA_DIR/daemon.json" 2>/dev/null)
  [ -n "$pid" ] && [ "$pid" != null ] && kill -9 "$pid" 2>/dev/null
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
check P2 "daemon start (resident, fresh data dir)" "$(is "$rc/$(grep -c '^daemon started (pid ' "$WORK/p2.out")" 0/1)" "rc=$rc; $(one "$WORK/p2.out")"
PORT=$(jq -r .port "$AGENT24_DATA_DIR/daemon.json" 2>/dev/null)
TOKEN=$(jq -r .token "$AGENT24_DATA_DIR/daemon.json" 2>/dev/null)
# A module finishes its handshake after `daemon start` returns; until then the
# OS answers module_not_ready (the import client's three tries are back to back).
t0=$(perl -MTime::HiRes=time -e 'printf "%d", time * 1000'); ready=fail
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
check U6 "--host-stdin --json (the desktop's way): the same job" "$(is "$rc/$(last "$WORK/u6.out" '.data.job_id')" "0/$JOB1")" "rc=$rc; $(tail -n 1 "$WORK/u6.out" | cut -c1-100)"

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
chmod 700 "$TMPBLOBS"
check C1 "the blob store not writable: the import fails, storage_unavailable" \
  "$(is "$stf/$stc/$(last "$WORK/c1.json" '"\(.data.error.code)/\(.data.attempt)"')" failed/failed/storage_unavailable/1)" \
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
check C5 "list: the two retried imports are documents now" "$(is "$rc/$(last "$WORK/c5.out" '.data.documents | length')" 0/4)" "rc=$rc; documents=$(last "$WORK/c5.out" '.data.documents | length')"

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

snapshot > "$WORK/.real-after"
check Z1 "the user's ~/.agent24 was not touched" "$(cmp -s "$WORK/.real-before" "$WORK/.real-after" && echo ok || echo fail)" \
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
