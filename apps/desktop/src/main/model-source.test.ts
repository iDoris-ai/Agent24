import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { describe, expect, it } from 'vitest'
import { clearModelSource, modelSourcePath, normalizeModelName, normalizeModelSourceUrl, readModelSource, writeModelSource } from './model-source'

describe('model source setting', () => {
  it('keeps a bare loopback origin, dropping a trailing /v1 or /', () => {
    expect(normalizeModelSourceUrl('http://127.0.0.1:1234')).toBe('http://127.0.0.1:1234')
    expect(normalizeModelSourceUrl(' http://127.0.0.1:1234/v1/ ')).toBe('http://127.0.0.1:1234')
    expect(normalizeModelSourceUrl('http://localhost:1234/')).toBe('http://localhost:1234')
    for (const bad of ['http://10.0.0.5:1234', 'http://localhost.evil:1234', 'http://127.0.0.1.nip.io:1234',
      'http://u:p@127.0.0.1:1234', 'http://127.0.0.1:1234/api', 'http://127.0.0.1:1234?x=1', 'ftp://127.0.0.1', '', 42]) {
      expect(normalizeModelSourceUrl(bad)).toBeNull()
    }
    expect(normalizeModelName(' qwen ')).toBe('qwen')
    expect(normalizeModelName('')).toBeNull()
    expect(normalizeModelName('a\nb')).toBeNull()
  })

  it('writes owner-only, reads back, and clears', () => {
    const dir = fs.mkdtempSync(path.join(process.env.TMPDIR ?? os.tmpdir(), 'a24-ms-'))
    const file = modelSourcePath(dir)
    expect(readModelSource(file)).toBeNull()
    writeModelSource(file, { url: 'http://127.0.0.1:1234', model: 'minicpm5-2b' })
    expect(fs.statSync(file).mode & 0o777).toBe(0o600)
    expect(readModelSource(file)).toEqual({ url: 'http://127.0.0.1:1234', model: 'minicpm5-2b' })
    clearModelSource(file)
    expect(readModelSource(file)).toBeNull()
    fs.rmSync(dir, { recursive: true, force: true })
  })
})
