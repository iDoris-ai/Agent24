#!/bin/sh
# Builds the slice-1 read engine (ADR-DOC-01 D6 amendment) into $1 (default:
# the current directory). macOS only: it links the system's PDFKit and Vision.
set -eu
out="${1:-.}"
here="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$out"
# The oldest macOS the helper is built for is fixed here. Left to swiftc, it
# is the build machine's own macOS, and the helper will not start on an older one.
xcrun swiftc -O -swift-version 5 -target "$(uname -m)-apple-macos13.0" \
  -framework PDFKit -framework Vision -framework AppKit -framework CoreImage \
  -o "$out/agent24-documents-pdfkit" "$here/main.swift" "$here/ocr.swift" "$here/render.swift"
