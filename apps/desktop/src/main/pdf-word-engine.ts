// L12-3 PDF → Word: the a24-pdf2docx engine the documents module converts with
// (rust/apps/agent24-documents/src/pdf_word.rs; built from tools/engines/pdf2docx/),
// downloaded on demand into the module's data directory, where its SBX-1
// sandbox can read and run it:
//   <data dir>/os/documents/engines/pdf2docx/<PDF2DOCX_VERSION>/
//     a24-pdf2docx/a24-pdf2docx   (from the pinned tar.gz, verified by sha256)
//     READY.json                  (written last; the module looks for it)
// Apple silicon only (the engine is built for arm64).
//
// Until the engine is published (after the AGPL licence review, ADR-DOC-03
// D2), a build machine hands the tarball over and the app is started with
// A24_PDF2DOCX_ENGINE_FILE=<path>: that file is copied instead of downloaded,
// and its sha256 is checked against the same pin.

import fs from 'node:fs'
import path from 'node:path'
import type { OfficeEngineStatus } from '../shared/ipc-types'
import type { PinnedFile } from './office-engine'

export const PDF2DOCX_VERSION = '0.5.13-a24.1'

/** The engine build (tools/engines/pdf2docx/build.sh manifest). */
export const PDF2DOCX_TARBALL: PinnedFile = {
  url: `https://github.com/iDoris-ai/Agent24/releases/download/engine-pdf2docx-${PDF2DOCX_VERSION}/a24-pdf2docx-${PDF2DOCX_VERSION}-darwin-arm64.tar.gz`,
  sha256: '9a2e828623db399bff5662ee49f7d3a009eddeac68bd093face6a2c8a7d07734',
  size: 101_330_244,
}

export function pdfWordEngineDir(dataDir: string): string {
  return path.join(dataDir, 'os', 'documents', 'engines', 'pdf2docx', PDF2DOCX_VERSION)
}

/** What the installer needs from the system; tests put their own. */
export interface PdfWordEngineDeps {
  dataDir: () => string | null
  arch: string
  /** A local copy of the tarball to install from instead of downloading (A24_PDF2DOCX_ENGINE_FILE), or null. */
  localFile: string | null
  /** Streams `url` to `dest`, calling `progress(bytesSoFar)`; rejects on HTTP or network failure. */
  download: (url: string, dest: string, progress: (bytes: number) => void, signal: AbortSignal) => Promise<void>
  /** sha256 hex of a file. */
  sha256: (file: string) => Promise<string>
  /** `tar -xzf <tarball> -C <destDir>`. */
  extractTar: (tarball: string, destDir: string) => Promise<void>
}

/** Downloads (or copies) and installs once; status() is what the page shows. */
export class PdfWordEngine {
  constructor(private readonly deps: PdfWordEngineDeps) {}

  status(): OfficeEngineStatus {
    void this.deps
    void fs
    throw new Error('L12-3: todo')
  }

  /** Starts (or joins) the install; resolves with the final status. */
  install(): Promise<OfficeEngineStatus> {
    throw new Error('L12-3: todo')
  }

  onChange(listener: (status: OfficeEngineStatus) => void): () => void {
    void listener
    throw new Error('L12-3: todo')
  }
}

/** The installer the app runs: fetch (or the local file), sha256, and tar. */
export function realPdfWordEngineDeps(dataDir: () => string | null): PdfWordEngineDeps {
  void dataDir
  throw new Error('L12-3: todo')
}
