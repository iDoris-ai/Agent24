#!/bin/sh
# Builds the slice-1 read engine (ADR-DOC-01 D6 amendment) into $1 (default:
# the current directory). macOS only: it links the system's PDFKit and Vision.
set -eu
out="${1:-.}"
here="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$out"
xcrun swiftc -O -swift-version 5 \
  -framework PDFKit -framework Vision -framework AppKit -framework CoreImage \
  -o "$out/agent24-documents-pdfkit" "$here/main.swift" "$here/ocr.swift" "$here/render.swift"
