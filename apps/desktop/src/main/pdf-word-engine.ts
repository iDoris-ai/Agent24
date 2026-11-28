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

import { execFile as execFileCb } from 'node:child_process'
import fs from 'node:fs'
import { copyFile, mkdir, rename, rm, stat, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { promisify } from 'node:util'
import type { OfficeEngineStatus } from '../shared/ipc-types'
import { downloadTo, sha256File, type PinnedFile } from './office-engine'

const execFile = promisify(execFileCb)

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
  private readonly listeners = new Set<(status: OfficeEngineStatus) => void>()
  private inflight: Promise<OfficeEngineStatus> | null = null
  /** True only while an install is in progress this run. */
  private active = false
  private received = 0
  private total: number | undefined
  /** Why the last install in this run failed, if it did. */
  private failure: string | null = null
  private lastNote = 0
  private noteTimer: ReturnType<typeof setTimeout> | null = null

  constructor(private readonly deps: PdfWordEngineDeps) {}

  status(): OfficeEngineStatus {
    if (this.active) {
      const status: OfficeEngineStatus = { state: 'installing', received: this.received }
      if (this.total !== undefined) status.total = this.total
      return status
    }
    const dataDir = this.deps.dataDir()
    if (dataDir === null) return { state: 'failed', error: 'no data directory' }
    if (fs.existsSync(path.join(pdfWordEngineDir(dataDir), 'READY.json'))) return { state: 'ready' }
    if (this.failure !== null) return { state: 'failed', error: this.failure }
    return { state: 'missing' }
  }

  /** Starts (or joins) the install; resolves with the final status. */
  install(): Promise<OfficeEngineStatus> {
    if (this.inflight) return this.inflight
    const run = this.execute().finally(() => {
      this.active = false
      if (this.noteTimer) {
        clearTimeout(this.noteTimer)
        this.noteTimer = null
      }
      if (this.inflight === run) this.inflight = null
    })
    this.inflight = run
    return run
  }

  onChange(listener: (status: OfficeEngineStatus) => void): () => void {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }

  private emit(status: OfficeEngineStatus): void {
    if (this.noteTimer) {
      clearTimeout(this.noteTimer)
      this.noteTimer = null
    }
    this.lastNote = Date.now()
    for (const listener of [...this.listeners]) listener(status)
  }

  /** Progress changes notify at most once every 200 ms. */
  private noteProgress(received: number): void {
    this.received = received
    const since = Date.now() - this.lastNote
    if (since >= 200) {
      this.emit(this.status())
      return
    }
    if (this.noteTimer) return
    this.noteTimer = setTimeout(() => {
      this.noteTimer = null
      if (!this.active) return
      this.emit(this.status())
    }, 200 - since)
  }

  private fail(error: string): OfficeEngineStatus {
    this.active = false
    this.failure = error
    const status: OfficeEngineStatus = { state: 'failed', error }
    this.emit(status)
    return status
  }

  private async execute(): Promise<OfficeEngineStatus> {
    const dataDir = this.deps.dataDir()
    if (dataDir === null) return this.fail('no data directory')
    if (this.deps.arch !== 'arm64') return this.fail('PDF to Word needs a Mac with Apple silicon')
    const finalDir = pdfWordEngineDir(dataDir)
    const work = `${finalDir}.partial`
    const tarball = path.join(work, 'engine.tar.gz')
    this.total = PDF2DOCX_TARBALL.size
    this.received = 0
    this.failure = null
    this.active = true
    this.emit(this.status())
    const ac = new AbortController()
    try {
      await rm(work, { recursive: true, force: true })
      await mkdir(work, { recursive: true })
      if (this.deps.localFile !== null) {
        await copyFile(this.deps.localFile, tarball)
        this.noteProgress((await stat(tarball)).size)
      } else {
        await this.deps.download(PDF2DOCX_TARBALL.url, tarball, (bytes) => {
          this.noteProgress(bytes)
        }, ac.signal)
      }
      const hash = await this.deps.sha256(tarball)
      if (hash !== PDF2DOCX_TARBALL.sha256) throw new Error('sha256 mismatch: a24-pdf2docx tarball')
      await this.deps.extractTar(tarball, work)
      await rm(tarball)
      await writeFile(path.join(work, 'READY.json'), JSON.stringify({
        version: PDF2DOCX_VERSION,
        sha256: PDF2DOCX_TARBALL.sha256,
      }))
      await rm(finalDir, { recursive: true, force: true })
      await rename(work, finalDir)
      this.active = false
      const ready: OfficeEngineStatus = { state: 'ready' }
      this.emit(ready)
      return ready
    } catch (error) {
      ac.abort()
      await rm(work, { recursive: true, force: true }).catch(() => {})
      return this.fail(error instanceof Error ? error.message : String(error))
    }
  }
}

/** The installer the app runs: fetch (or the local file), sha256, and tar. */
export function realPdfWordEngineDeps(dataDir: () => string | null): PdfWordEngineDeps {
  return {
    dataDir,
    arch: process.arch,
    localFile: process.env.A24_PDF2DOCX_ENGINE_FILE || null,
    download: downloadTo,
    sha256: sha256File,
    extractTar: async (tarball, destDir) => {
      await execFile('/usr/bin/tar', ['-xzf', tarball, '-C', destDir])
    },
  }
}
