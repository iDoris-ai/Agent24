// D1 Office → PDF: the LibreOffice build the documents module converts with
// (rust/apps/agent24-documents/src/office.rs), downloaded on demand into the
// module's data directory, where its SBX-1 sandbox can read and run it:
//   <data dir>/os/documents/engines/libreoffice/<LIBREOFFICE_VERSION>/
//     LibreOffice.app/   (from the pinned DMG, verified by sha256)
//     fonts/*.otf        (Noto Sans SC / Noto Serif SC, OFL, pinned by sha256)
//     READY.json         (written last; the module looks for it)
// Design and the measurements behind it:
// stop-the-world/ui-materials/2026-11-18-d1-office-to-pdf-design.md

import crypto from 'node:crypto'
import { execFile as execFileCb } from 'node:child_process'
import { once } from 'node:events'
import fs from 'node:fs'
import { mkdir, mkdtemp, rename, rm, stat, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { promisify } from 'node:util'
import type { OfficeEngineStatus } from '../shared/ipc-types'

const execFile = promisify(execFileCb)

export const LIBREOFFICE_VERSION = '26.8.3'

export interface PinnedFile { url: string; sha256: string; size: number }

const LO_BASE = `https://download.documentfoundation.org/libreoffice/stable/${LIBREOFFICE_VERSION}/mac`

/** The DMG by CPU (`process.arch`). */
export const LIBREOFFICE_DMG: Record<'arm64' | 'x64', PinnedFile> = {
  arm64: {
    url: `${LO_BASE}/aarch64/LibreOffice_${LIBREOFFICE_VERSION}_MacOS_aarch64.dmg`,
    sha256: 'de943203795e5057785f65515063e2db5eb60b0bb6d8e2534fec59e5cc5fb988',
    size: 299_927_705,
  },
  x64: {
    url: `${LO_BASE}/x86_64/LibreOffice_${LIBREOFFICE_VERSION}_MacOS_x86-64.dmg`,
    sha256: 'bb3c35d628780510915ad9e759d7ed898094d7985d01b2181bd1d4ecd94fd159',
    size: 0, // unknown until downloaded: the sha256 is what is checked
  },
}

const NOTO = 'https://github.com/notofonts/noto-cjk/raw'

/** ADR-DOC-03 D4: the fonts the replacement table names. */
export const OFFICE_FONTS: Array<PinnedFile & { name: string }> = [
  { name: 'NotoSansSC-Regular.otf', url: `${NOTO}/Sans2.004/Sans/SubsetOTF/SC/NotoSansSC-Regular.otf`, sha256: 'faa6c9df652116dde789d351359f3d7e5d2285a2b2a1f04a2d7244df706d5ea9', size: 8_331_336 },
  { name: 'NotoSansSC-Bold.otf', url: `${NOTO}/Sans2.004/Sans/SubsetOTF/SC/NotoSansSC-Bold.otf`, sha256: 'c6cb5a93abaa9edc8ee7463b7ebb7f42d618d40e6ed2f7a5371c97b0b64767c0', size: 8_543_168 },
  { name: 'NotoSerifSC-Regular.otf', url: `${NOTO}/Serif2.003/Serif/SubsetOTF/SC/NotoSerifSC-Regular.otf`, sha256: 'e8f396decc1f0963a016a989c3d8852e863d1350996f573860a80767c83a1cd3', size: 11_625_800 },
  { name: 'NotoSerifSC-Bold.otf', url: `${NOTO}/Serif2.003/Serif/SubsetOTF/SC/NotoSerifSC-Bold.otf`, sha256: '24693d48bdb9152f0a06b02af625638a1097abd6de4010ebba027f6e82710527', size: 12_094_336 },
]

export function officeEngineDir(dataDir: string): string {
  return path.join(dataDir, 'os', 'documents', 'engines', 'libreoffice', LIBREOFFICE_VERSION)
}

/** What the installer needs from the system; tests put their own. */
export interface OfficeEngineDeps {
  dataDir: () => string | null
  arch: string
  /** Streams `url` to `dest`, calling `progress(bytesSoFar)`; rejects on HTTP or network failure. */
  download: (url: string, dest: string, progress: (bytes: number) => void, signal: AbortSignal) => Promise<void>
  /** sha256 hex of a file. */
  sha256: (file: string) => Promise<string>
  /** `hdiutil attach -nobrowse -readonly`, `ditto <mount>/LibreOffice.app <dest>`, `hdiutil detach`. */
  extractApp: (dmg: string, destApp: string) => Promise<void>
}

/** Downloads and installs once; status() is what the page shows. */
export class OfficeEngine {
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

  constructor(private readonly deps: OfficeEngineDeps) {}

  status(): OfficeEngineStatus {
    if (this.active) {
      const status: OfficeEngineStatus = { state: 'installing', received: this.received }
      if (this.total !== undefined) status.total = this.total
      return status
    }
    const dataDir = this.deps.dataDir()
    if (dataDir === null) return { state: 'failed', error: 'no data directory' }
    if (fs.existsSync(path.join(officeEngineDir(dataDir), 'READY.json'))) return { state: 'ready' }
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
    const finalDir = officeEngineDir(dataDir)
    const work = `${finalDir}.partial`
    const dmg = this.deps.arch === 'arm64' ? LIBREOFFICE_DMG.arm64 : LIBREOFFICE_DMG.x64
    const items = [
      { url: dmg.url, sha256: dmg.sha256, dest: path.join(work, 'LibreOffice.dmg'), name: 'LibreOffice.dmg' },
      ...OFFICE_FONTS.map((font) => ({
        url: font.url, sha256: font.sha256, dest: path.join(work, 'fonts', font.name), name: font.name,
      })),
    ]
    this.total = dmg.size === 0 ? undefined : dmg.size + OFFICE_FONTS.reduce((sum, font) => sum + font.size, 0)
    this.received = 0
    this.failure = null
    this.active = true
    this.emit(this.status())
    const ac = new AbortController()
    try {
      await rm(work, { recursive: true, force: true })
      await mkdir(path.join(work, 'fonts'), { recursive: true })
      let done = 0
      for (const item of items) {
        let current = 0
        await this.deps.download(item.url, item.dest, (bytes) => {
          current = bytes
          this.noteProgress(done + bytes)
        }, ac.signal)
        if (current === 0) {
          current = (await stat(item.dest)).size
          this.noteProgress(done + current)
        }
        done += current
        const hash = await this.deps.sha256(item.dest)
        if (hash !== item.sha256) throw new Error(`sha256 mismatch: ${item.name}`)
      }
      const dmgPath = path.join(work, 'LibreOffice.dmg')
      await this.deps.extractApp(dmgPath, path.join(work, 'LibreOffice.app'))
      await rm(dmgPath)
      await writeFile(path.join(work, 'READY.json'), JSON.stringify({
        version: LIBREOFFICE_VERSION,
        dmg_sha256: dmg.sha256,
        fonts: OFFICE_FONTS.map((font) => font.name),
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

/** The installer the app runs: fetch, sha256, and hdiutil. */
export function realOfficeEngineDeps(dataDir: () => string | null): OfficeEngineDeps {
  return {
    dataDir,
    arch: process.arch,
    download: async (url, dest, progress, signal) => {
      const res = await fetch(url, { redirect: 'follow', signal })
      if (!res.ok) {
        await res.body?.cancel().catch(() => {})
        throw new Error(`HTTP ${res.status}`)
      }
      if (!res.body) throw new Error('empty body')
      await mkdir(path.dirname(dest), { recursive: true })
      const ws = fs.createWriteStream(dest)
      const reader = res.body.getReader()
      const broken = new Promise<never>((_, reject) => { ws.on('error', reject) })
      let received = 0
      try {
        for (;;) {
          const { done, value } = await Promise.race([reader.read(), broken])
          if (done) break
          received += value.byteLength
          if (!ws.write(value)) await Promise.race([once(ws, 'drain').then(() => undefined), broken])
          progress(received)
        }
        await Promise.race([
          new Promise<void>((resolve, reject) => { ws.end((error?: Error | null) => (error ? reject(error) : resolve())) }),
          broken,
        ])
      } catch (error) {
        ws.destroy()
        await reader.cancel().catch(() => {})
        throw error
      }
    },
    sha256: (file) => new Promise((resolve, reject) => {
      const hash = crypto.createHash('sha256')
      fs.createReadStream(file)
        .on('data', (chunk) => hash.update(chunk))
        .on('error', reject)
        .on('end', () => resolve(hash.digest('hex')))
    }),
    extractApp: async (dmg, destApp) => {
      const parent = await mkdtemp(path.join(path.dirname(dmg), '..', 'mount-'))
      let mount: string | null = null
      try {
        const attached = await execFile('hdiutil', ['attach', '-nobrowse', '-readonly', '-mountrandom', parent, dmg])
        const points = String(attached.stdout).split(/\s+/).filter((part) => part.startsWith('/'))
        for (const point of points) {
          try {
            if ((await stat(path.join(point, 'LibreOffice.app'))).isDirectory()) {
              mount = point
              break
            }
          } catch { /* a /dev node, or not this volume */ }
        }
        if (!mount) throw new Error('LibreOffice.app was not in the disk image')
        await execFile('ditto', [path.join(mount, 'LibreOffice.app'), destApp])
      } finally {
        if (mount) await execFile('hdiutil', ['detach', mount, '-force']).catch(() => {})
        await rm(parent, { recursive: true, force: true }).catch(() => {})
      }
    },
  }
}
