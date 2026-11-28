import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { PDF2DOCX_TARBALL, PDF2DOCX_VERSION, PdfWordEngine, pdfWordEngineDir, type PdfWordEngineDeps } from './pdf-word-engine'
import type { OfficeEngineStatus } from '../shared/ipc-types'

const roots: string[] = []
afterEach(() => {
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true })
  roots.length = 0
})

function tempDir(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'pdf-word-engine-'))
  roots.push(dir)
  return dir
}

const writeDownload: PdfWordEngineDeps['download'] = async (_url, dest, progress) => {
  fs.mkdirSync(path.dirname(dest), { recursive: true })
  const bytes = Buffer.from('tarball')
  fs.writeFileSync(dest, bytes)
  progress(bytes.length)
}

/** Stands in for `tar -xzf`: the engine program at its place in the archive. */
const fakeExtract: PdfWordEngineDeps['extractTar'] = async (_tarball, destDir) => {
  fs.mkdirSync(path.join(destDir, 'a24-pdf2docx'), { recursive: true })
  fs.writeFileSync(path.join(destDir, 'a24-pdf2docx', 'a24-pdf2docx'), '#!/bin/sh\n')
}

function depsFor(dataDir: string, over: Partial<PdfWordEngineDeps> = {}): PdfWordEngineDeps {
  return {
    dataDir: () => dataDir,
    arch: 'arm64',
    localFile: null,
    download: writeDownload,
    sha256: async () => PDF2DOCX_TARBALL.sha256,
    extractTar: fakeExtract,
    ...over,
  }
}

describe('PdfWordEngine', () => {
  it('is missing until installed, then installs into the final directory and reports ready', async () => {
    const dir = tempDir()
    const seen: OfficeEngineStatus[] = []
    const engine = new PdfWordEngine(depsFor(dir))
    expect(engine.status()).toEqual({ state: 'missing' })
    engine.onChange((status) => seen.push(status))
    expect(await engine.install()).toEqual({ state: 'ready' })
    const finalDir = pdfWordEngineDir(dir)
    expect(finalDir).toBe(path.join(dir, 'os', 'documents', 'engines', 'pdf2docx', PDF2DOCX_VERSION))
    expect(fs.existsSync(path.join(finalDir, 'a24-pdf2docx', 'a24-pdf2docx'))).toBe(true)
    expect(fs.existsSync(`${finalDir}.partial`)).toBe(false)
    // The tarball itself is not kept.
    expect(fs.readdirSync(finalDir).sort()).toEqual(['READY.json', 'a24-pdf2docx'])
    expect(JSON.parse(fs.readFileSync(path.join(finalDir, 'READY.json'), 'utf8'))).toEqual({
      version: PDF2DOCX_VERSION,
      sha256: PDF2DOCX_TARBALL.sha256,
    })
    expect(engine.status()).toEqual({ state: 'ready' })
    expect(seen.some((s) => s.state === 'installing')).toBe(true)
    expect(seen.at(-1)).toEqual({ state: 'ready' })
  })

  it('a sha256 mismatch fails and leaves no install', async () => {
    const dir = tempDir()
    let extracted = false
    const engine = new PdfWordEngine(depsFor(dir, {
      sha256: async () => 'not-the-pin',
      extractTar: async (t, d) => { extracted = true; await fakeExtract(t, d) },
    }))
    const status = await engine.install()
    expect(status.state).toBe('failed')
    expect(status.error).toContain('sha256')
    expect(extracted).toBe(false)
    expect(fs.existsSync(pdfWordEngineDir(dir))).toBe(false)
    expect(fs.existsSync(`${pdfWordEngineDir(dir)}.partial`)).toBe(false)
    expect(engine.status().state).toBe('failed')
  })

  it('installs from the local file instead of downloading, still checking the pin', async () => {
    const dir = tempDir()
    const local = path.join(tempDir(), 'engine.tar.gz')
    fs.writeFileSync(local, 'local tarball')
    let downloads = 0
    const hashed: string[] = []
    const engine = new PdfWordEngine(depsFor(dir, {
      localFile: local,
      download: async (...args) => { downloads += 1; await writeDownload(...args) },
      sha256: async (file) => { hashed.push(fs.readFileSync(file, 'utf8')); return PDF2DOCX_TARBALL.sha256 },
    }))
    expect(await engine.install()).toEqual({ state: 'ready' })
    expect(downloads).toBe(0)
    expect(hashed).toEqual(['local tarball'])
    // The local file is left where it was.
    expect(fs.existsSync(local)).toBe(true)

    const bad = new PdfWordEngine(depsFor(tempDir(), { localFile: local, sha256: async () => 'other' }))
    expect((await bad.install()).state).toBe('failed')
  })

  it('is not offered off Apple silicon', async () => {
    const dir = tempDir()
    let downloads = 0
    const engine = new PdfWordEngine(depsFor(dir, { arch: 'x64', download: async (...args) => { downloads += 1; await writeDownload(...args) } }))
    const status = await engine.install()
    expect(status.state).toBe('failed')
    expect(status.error).toMatch(/Apple/)
    expect(downloads).toBe(0)
  })

  it('a failed extraction leaves no install', async () => {
    const dir = tempDir()
    const engine = new PdfWordEngine(depsFor(dir, { extractTar: async () => { throw new Error('tar exited 1') } }))
    const status = await engine.install()
    expect(status).toEqual({ state: 'failed', error: 'tar exited 1' })
    expect(fs.existsSync(pdfWordEngineDir(dir))).toBe(false)
    expect(fs.existsSync(`${pdfWordEngineDir(dir)}.partial`)).toBe(false)
  })

  it('two installs at once download once', async () => {
    const dir = tempDir()
    let downloads = 0
    const engine = new PdfWordEngine(depsFor(dir, { download: async (...args) => { downloads += 1; await writeDownload(...args) } }))
    const first = engine.install()
    const second = engine.install()
    expect(second).toBe(first)
    expect(await first).toEqual(await second)
    expect(downloads).toBe(1)
  })
})
