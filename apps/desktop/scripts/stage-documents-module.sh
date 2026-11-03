#!/usr/bin/env bash
# Stages the documents OS module package (ADR-DOC-01 D6: shipped inside the
# installer, pinned by sha256) into apps/desktop/.documents-module/, which
# package.json's mac `extraResources` copies to
# Agent24.app/Contents/Resources/documents-module/.
#
# The layout is the one `agent24 os install <dir>` takes, the same four
# commands scripts/doc1-r6-accept.sh and docs/release/ACCEPTANCE-v0.6.0-preview.md
# use:
#   domain-os.yml
#   bin/agent24-documents          (cargo build --release -p agent24-documents)
#   bin/agent24-documents-pdfkit   (engines/pdfkit/build.sh; macOS only)
#   SHA256SUMS                     (sha256 of the three files above)
#
# Usage: stage-documents-module.sh [<cargo release dir>]
#   default: <repo>/rust/target/release, the directory the mac extraResources
#   entries for agent24d / agent24 read from.
# macOS only: the PDF engine links PDFKit and Vision.
#
# A mac build that never ran this ships no Resources/documents-module/ at all:
# electron-builder only warns ("file source doesn't exist") for a missing
# extraResources source, and leaves an empty one out of the app.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
desktop="$(cd "$here/.." && pwd)"
repo="$(cd "$desktop/../.." && pwd)"
cargo_out="${1:-$repo/rust/target/release}"
module_src="$repo/rust/apps/agent24-documents"
out="$desktop/.documents-module"

die() { echo "stage-documents-module: $*" >&2; exit 1; }

[[ "$(uname -s)" == Darwin ]] || die "macOS only (the pdfkit engine links PDFKit and Vision)"
[[ -x "$cargo_out/agent24-documents" ]] \
  || die "missing $cargo_out/agent24-documents (run: cargo build --release -p agent24-documents)"

# Build into a fresh sibling directory and swap it in at the end, so a failed
# run never leaves a half-staged package where electron-builder would pick it up.
tmp="$(mktemp -d "$desktop/.documents-module.tmp.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
chmod 0755 "$tmp"  # mktemp -d makes it 0700; it ships as Resources/documents-module/
mkdir -p "$tmp/bin"
cp "$module_src/domain-os.yml" "$tmp/domain-os.yml"
cp "$cargo_out/agent24-documents" "$tmp/bin/agent24-documents"
chmod 0755 "$tmp/bin/agent24-documents"
"$module_src/engines/pdfkit/build.sh" "$tmp/bin"
[[ -x "$tmp/bin/agent24-documents-pdfkit" ]] || die "the pdfkit engine did not build"

( cd "$tmp" && shasum -a 256 domain-os.yml bin/agent24-documents bin/agent24-documents-pdfkit > SHA256SUMS \
  && shasum -a 256 -c --quiet SHA256SUMS ) || die "SHA256SUMS did not verify"

rm -rf "$out"
mv "$tmp" "$out"
trap - EXIT
version="$(sed -n 's/^version: *"\{0,1\}\([^"]*\)"\{0,1\} *$/\1/p' "$out/domain-os.yml")"
echo "staged documents module ${version:-?} into $out"
cat "$out/SHA256SUMS"
