import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { LIBREOFFICE_DMG, LIBREOFFICE_VERSION, OFFICE_FONTS, OfficeEngine, officeEngineDir, type OfficeEngineDeps, type OfficeEngineStatus } from './office-engine'

const roots: string[] = []
afterEach(() => {
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true })
  roots.length = 0
})

function tempDir(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'office-engine-'))
  roots.push(dir)
  return dir
}

function shaOf(file: string): string {
  const base = path.basename(file)
  if (base === 'LibreOffice.dmg') return LIBREOFFICE_DMG.arm64.sha256
  return OFFICE_FONTS.find((font) => font.name === base)?.sha256 ?? 'missing'
}

function depsFor(dataDir: string, download: OfficeEngineDeps['download'], sha256: OfficeEngineDeps['sha256'] = async (file) => shaOf(file)): OfficeEngineDeps {
  return {
    dataDir: () => dataDir,
    arch: 'arm64',
    download,
    sha256,
    extractApp: async (_dmg, destApp) => { fs.mkdirSync(destApp) },
  }
}

const writeDownload: OfficeEngineDeps['download'] = async (_url, dest, progress) => {
  fs.mkdirSync(path.dirname(dest), { recursive: true })
  const bytes = Buffer.from('hello')
  fs.writeFileSync(dest, bytes)
  progress(bytes.length)
}

describe('OfficeEngine', () => {
  it('installs into the final directory and reports ready', async () => {
    const dir = tempDir()
    const seen: OfficeEngineStatus[] = []
    const engine = new OfficeEngine(depsFor(dir, writeDownload))
    engine.onChange((status) => seen.push(status))
    expect(await engine.install()).toEqual({ state: 'ready' })
    const finalDir = officeEngineDir(dir)
    expect(fs.existsSync(finalDir)).toBe(true)
    expect(fs.existsSync(`${finalDir}.partial`)).toBe(false)
    expect(JSON.parse(fs.readFileSync(path.join(finalDir, 'READY.json'), 'utf8'))).toEqual({
      version: LIBREOFFICE_VERSION,
      dmg_sha256: LIBREOFFICE_DMG.arm64.sha256,
      fonts: OFFICE_FONTS.map((font) => font.name),
    })
    expect(engine.status()).toEqual({ state: 'ready' })
    expect(seen.length).toBeGreaterThan(0)
  })

  it('a sha256 mismatch fails and leaves no install', async () => {
    const dir = tempDir()
    const engine = new OfficeEngine(depsFor(dir, writeDownload, async () => 'not-the-pin'))
    const status = await engine.install()
    expect(status.state).toBe('failed')
    expect(status.error).toContain('LibreOffice.dmg')
    expect(fs.existsSync(officeEngineDir(dir))).toBe(false)
    expect(fs.existsSync(`${officeEngineDir(dir)}.partial`)).toBe(false)
    expect(engine.status().state).toBe('failed')
  })

  it('two installs at once download once', async () => {
    const dir = tempDir()
    let downloads = 0
    const engine = new OfficeEngine(depsFor(dir, async (url, dest, progress, signal) => {
      downloads += 1
      await writeDownload(url, dest, progress, signal)
    }))
    const first = engine.install()
    const second = engine.install()
    expect(second).toBe(first)
    expect(await first).toEqual(await second)
    expect(downloads).toBe(1 + OFFICE_FONTS.length)
  })
})
