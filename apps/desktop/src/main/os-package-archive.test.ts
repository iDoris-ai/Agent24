import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import zlib from 'node:zlib'
import { afterEach, describe, expect, it } from 'vitest'
import { ArchiveRejected, DEFAULT_ARCHIVE_LIMITS, extractTarGz } from './os-package-archive'

const dirs: string[] = []

function tempDir(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'os-archive-'))
  dirs.push(dir)
  return dir
}

afterEach(() => {
  for (const dir of dirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true })
})

function header(name: string, size: number, type: string, mode = '0000644'): Buffer {
  const block = Buffer.alloc(512)
  block.write(name)
  block.write(`${mode}\0`, 100)
  block.write('0000000\0', 108)
  block.write('0000000\0', 116)
  block.write(`${size.toString(8).padStart(11, '0')}\0`, 124)
  block.write('00000000000\0', 136)
  block.write('        ', 148)
  block.write(type, 156)
  block.write('ustar\0', 257)
  block.write('00', 263)
  let sum = 0
  for (const byte of block) sum += byte
  block.write(`${sum.toString(8).padStart(6, '0')}\0 `, 148)
  return block
}

function archive(entries: { name: string; type: string; body?: string; mode?: string }[]): string {
  const parts: Buffer[] = []
  for (const entry of entries) {
    const body = Buffer.from(entry.body ?? '')
    parts.push(header(entry.name, entry.type === '5' ? 0 : body.length, entry.type, entry.mode))
    if (entry.type !== '5' && body.length > 0) {
      parts.push(body, Buffer.alloc((512 - (body.length % 512)) % 512))
    }
  }
  parts.push(Buffer.alloc(1024))
  const file = path.join(tempDir(), 'pkg.tar.gz')
  fs.writeFileSync(file, zlib.gzipSync(Buffer.concat(parts)))
  return file
}

describe('extractTarGz', () => {
  it('unpacks a package directory', () => {
    const dest = tempDir()
    extractTarGz(archive([
      { name: 'sin90-0.5.2-macos-arm64', type: '5' },
      { name: 'sin90-0.5.2-macos-arm64/domain-os.yml', type: '0', body: 'name: sin90\nversion: 0.5.2\n' },
    ]), dest)
    expect(fs.readFileSync(path.join(dest, 'sin90-0.5.2-macos-arm64/domain-os.yml'), 'utf8')).toContain('version: 0.5.2')
  })

  it('keeps the packaged permission bits and drops setuid', () => {
    const dest = tempDir()
    extractTarGz(archive([
      { name: 'pkg/bin/sin90', type: '0', body: 'bin', mode: '0004755' },
      { name: 'pkg/domain-os.yml', type: '0', body: 'name: sin90\n', mode: '0000444' },
    ]), dest)
    expect(fs.statSync(path.join(dest, 'pkg/bin/sin90')).mode & 0o7777).toBe(0o755)
    expect(fs.statSync(path.join(dest, 'pkg/domain-os.yml')).mode & 0o7777).toBe(0o444)
  })

  it.each(['../outside.txt', '/tmp/outside.txt', 'pkg/../../outside.txt', 'link'])('refuses %s', (name) => {
    const dest = tempDir()
    const type = name === 'link' ? '2' : '0'
    const file = archive([{ name, type, body: name === 'link' ? '' : 'x' }])
    expect(() => extractTarGz(file, dest)).toThrow(ArchiveRejected)
    expect(fs.existsSync(path.join(dest, 'outside.txt'))).toBe(false)
    expect(fs.existsSync('/tmp/outside.txt') && name === '/tmp/outside.txt' ? fs.readFileSync('/tmp/outside.txt', 'utf8') : '').not.toBe('x')
  })

  it('caps gunzip output, entry count, and payload bytes at 1 GiB and 10000 entries', () => {
    expect(DEFAULT_ARCHIVE_LIMITS).toEqual({
      maxOutputLength: 1024 * 1024 * 1024,
      maxEntries: 10_000,
      maxBytes: 1024 * 1024 * 1024,
    })
  })

  it('rejects a gzip that expands past the limit', () => {
    const dest = tempDir()
    const file = path.join(tempDir(), 'bomb.tar.gz')
    fs.writeFileSync(file, zlib.gzipSync(Buffer.alloc(2000, 1)))
    expect(() => extractTarGz(file, dest, { maxOutputLength: 100, maxEntries: 10, maxBytes: 1000 }))
      .toThrow(/gzip expands past the size limit/)
    expect(fs.readdirSync(dest)).toEqual([])
  })

  it('rejects an archive with more entries than the limit', () => {
    const dest = tempDir()
    const file = archive([
      { name: 'a.txt', type: '0', body: 'a' },
      { name: 'b.txt', type: '0', body: 'b' },
      { name: 'c.txt', type: '0', body: 'c' },
    ])
    expect(() => extractTarGz(file, dest, { maxOutputLength: 1_000_000, maxEntries: 2, maxBytes: 1_000_000 }))
      .toThrow(/too many entries/)
    expect(fs.existsSync(path.join(dest, 'c.txt'))).toBe(false)
  })

  it('rejects an archive whose payload bytes pass the limit', () => {
    const dest = tempDir()
    const file = archive([{ name: 'a.txt', type: '0', body: 'hello world' }])
    expect(() => extractTarGz(file, dest, { maxOutputLength: 1_000_000, maxEntries: 10, maxBytes: 4 }))
      .toThrow(/expands past the size limit/)
    expect(fs.existsSync(path.join(dest, 'a.txt'))).toBe(false)
  })
})
