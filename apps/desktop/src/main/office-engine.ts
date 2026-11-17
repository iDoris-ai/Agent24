// D1 Office → PDF: the LibreOffice build the documents module converts with
// (rust/apps/agent24-documents/src/office.rs), downloaded on demand into the
// module's data directory, where its SBX-1 sandbox can read and run it:
//   <data dir>/os/documents/engines/libreoffice/<LIBREOFFICE_VERSION>/
//     LibreOffice.app/   (from the pinned DMG, verified by sha256)
//     fonts/*.otf        (Noto Sans SC / Noto Serif SC, OFL, pinned by sha256)
//     READY.json         (written last; the module looks for it)
// Design and the measurements behind it:
// stop-the-world/ui-materials/2026-11-18-d1-office-to-pdf-design.md

import path from 'node:path'
import type { OfficeEngineStatus } from '../shared/ipc-types'

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
  constructor(private readonly deps: OfficeEngineDeps) {}

  status(): OfficeEngineStatus {
    return { state: 'missing' }
  }

  /** Starts (or joins) the install; resolves with the final status. */
  install(): Promise<OfficeEngineStatus> {
    return Promise.resolve(this.status())
  }

  onChange(listener: (status: OfficeEngineStatus) => void): () => void {
    void listener
    return () => {}
  }
}
