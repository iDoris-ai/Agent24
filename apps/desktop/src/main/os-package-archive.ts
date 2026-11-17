// Unpack a release tar.gz into a private directory. Rejects absolute paths,
// `..`, and link entries so a tarball cannot write outside `dest`.

import fs from 'node:fs'
import path from 'node:path'
import zlib from 'node:zlib'

const BLOCK = 512
const GIBIBYTE = 1024 * 1024 * 1024

/** gzip output, payload bytes, and entry count. Past any of these is ArchiveRejected. */
export const DEFAULT_ARCHIVE_LIMITS = {
  maxOutputLength: GIBIBYTE,
  maxEntries: 10_000,
  maxBytes: GIBIBYTE,
}

export interface ArchiveLimits {
  maxOutputLength: number
  maxEntries: number
  maxBytes: number
}

export class ArchiveRejected extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'ArchiveRejected'
  }
}

function octal(buf: Buffer, offset: number, length: number): number {
  const text = buf.toString('utf8', offset, offset + length).replace(/\0.*$/, '').trim()
  if (text === '') return 0
  if (!/^[0-7]+$/.test(text)) throw new ArchiveRejected('tar header size is not octal')
  return parseInt(text, 8)
}

function headerChecksum(header: Buffer): number {
  let sum = 0
  for (let i = 0; i < BLOCK; i += 1) sum += i >= 148 && i < 156 ? 32 : header[i]!
  return sum
}

function field(header: Buffer, offset: number, length: number): string {
  return header.toString('utf8', offset, offset + length).replace(/\0.*$/, '')
}

/** A tar path that stays inside `root` after resolving. */
export function safeArchivePath(root: string, name: string): string {
  if (name === '' || name.includes('\\') || path.posix.isAbsolute(name) || path.win32.isAbsolute(name)) {
    throw new ArchiveRejected(`refusing archive path ${name}`)
  }
  const parts = name.split('/')
  if (parts.some((part) => part === '..' || part === '')) {
    throw new ArchiveRejected(`refusing archive path ${name}`)
  }
  const target = path.resolve(root, ...parts)
  if (target !== root && !target.startsWith(root + path.sep)) {
    throw new ArchiveRejected(`refusing archive path ${name}`)
  }
  return target
}

function assertNoSymlink(root: string, target: string): void {
  let current = root
  const rest = path.relative(root, target).split(path.sep).filter(Boolean)
  for (const part of rest) {
    current = path.join(current, part)
    let st: fs.Stats
    try {
      st = fs.lstatSync(current)
    } catch {
      continue
    }
    if (st.isSymbolicLink()) throw new ArchiveRejected(`refusing to follow a link at ${current}`)
  }
}

/** Extracts `file` (gzip + ustar) into `dest`, which must already exist. */
export function extractTarGz(file: string, dest: string, limits: ArchiveLimits = DEFAULT_ARCHIVE_LIMITS): void {
  const dirModes: [string, number][] = []
  extractEntries(file, dest, limits, dirModes)
  // Directory bits last and deepest first, so a read-only directory does not
  // block writing the files inside it. `os install` and `package-integrity
  // verify` both compare them with the package.
  for (const [dir, mode] of dirModes.sort((x, y) => y[0].length - x[0].length)) fs.chmodSync(dir, mode)
}

function extractEntries(file: string, dest: string, limits: ArchiveLimits, dirModes: [string, number][]): void {
  const raw = gunzipLimited(fs.readFileSync(file), limits.maxOutputLength)
  const root = fs.realpathSync(dest)
  let offset = 0
  let entries = 0
  let totalBytes = 0
  let longName: string | null = null
  while (offset + BLOCK <= raw.length) {
    const header = raw.subarray(offset, offset + BLOCK)
    offset += BLOCK
    if (header.every((byte) => byte === 0)) {
      if (longName !== null) throw new ArchiveRejected('tar ended during a long name')
      return
    }
    const stored = octal(header, 148, 8)
    if (stored !== headerChecksum(header)) throw new ArchiveRejected('tar header checksum mismatch')
    const type = String.fromCharCode(header[156] ?? 0)
    const size = octal(header, 124, 12)
    if (size < 0 || offset + size > raw.length) throw new ArchiveRejected('tar entry extends past the archive')
    entries += 1
    totalBytes += size
    if (entries > limits.maxEntries) throw new ArchiveRejected('archive has too many entries')
    if (totalBytes > limits.maxBytes) throw new ArchiveRejected('archive expands past the size limit')
    const data = raw.subarray(offset, offset + size)
    offset += Math.ceil(size / BLOCK) * BLOCK
    if (type === 'L') {
      longName = data.toString('utf8').replace(/\0+$/, '')
      continue
    }
    if (type === 'g' || type === 'x') {
      longName = null
      continue
    }
    let name = longName ?? field(header, 0, 100)
    const prefix = field(header, 345, 155)
    if (longName === null && prefix !== '') name = `${prefix}/${name}`
    longName = null
    if (type === '0' || type === '\0') {
      const target = safeArchivePath(root, name)
      fs.mkdirSync(path.dirname(target), { recursive: true })
      assertNoSymlink(root, target)
      fs.writeFileSync(target, data, { flag: 'wx' })
      // Keep the packaged permission bits: `os install` checks them against
      // files.json. Only rwx survives, never setuid / setgid / sticky.
      fs.chmodSync(target, octal(header, 100, 8) & 0o777)
    } else if (type === '5') {
      const target = safeArchivePath(root, name.replace(/\/$/, ''))
      fs.mkdirSync(target, { recursive: true })
      assertNoSymlink(root, target)
      dirModes.push([target, octal(header, 100, 8) & 0o777])
    } else if (type === '1' || type === '2') {
      throw new ArchiveRejected(`refusing link entry ${name}`)
    } else {
      throw new ArchiveRejected(`refusing tar entry type ${type} (${name})`)
    }
  }
  if (offset < raw.length && !raw.subarray(offset).every((byte) => byte === 0)) {
    throw new ArchiveRejected('tar ended mid-header')
  }
}

function gunzipLimited(compressed: Buffer, maxOutputLength: number): Buffer {
  try {
    return zlib.gunzipSync(compressed, { maxOutputLength })
  } catch (error) {
    const code = typeof error === 'object' && error !== null && 'code' in error
      ? String((error as { code: unknown }).code)
      : ''
    if (code === 'ERR_BUFFER_TOO_LARGE') throw new ArchiveRejected('gzip expands past the size limit')
    throw new ArchiveRejected(error instanceof Error ? error.message : 'gzip could not be read')
  }
}
