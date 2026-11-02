import { mkdtempSync, mkdirSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { execFileSync } from 'node:child_process'
import os from 'node:os'
import path from 'node:path'
import { afterAll, describe, expect, it } from 'vitest'
import { parseBackupText, readBackupFile } from './backup-file'

const GOOD = `ncryptsec1${'q'.repeat(150)}`
const dir = mkdtempSync(path.join(os.tmpdir(), 'backup-file-'))
afterAll(() => rmSync(dir, { recursive: true, force: true }))
const put = (name: string, body: string): string => {
  const p = path.join(dir, name)
  writeFileSync(p, body)
  return p
}

describe('readBackupFile (L2: read once, at pick time)', () => {
  it('reads one clean ncryptsec line, with or without its own line break', () => {
    expect(readBackupFile(put('a', `${GOOD}\n`))).toEqual({ ok: true, ncryptsec: GOOD })
    expect(readBackupFile(put('b', `${GOOD}\r\n`))).toEqual({ ok: true, ncryptsec: GOOD })
    expect(readBackupFile(put('c', GOOD))).toEqual({ ok: true, ncryptsec: GOOD })
    expect(readBackupFile(put('d', `${GOOD}\r`))).toEqual({ ok: true, ncryptsec: GOOD }) // a lone CR, like the old reader
    expect(parseBackupText(`${GOOD}\r\r`)).toBeNull()
  })

  it('refuses everything that is not exactly that', () => {
    for (const [name, body] of [
      ['plain', 'hello\n'],
      ['multi', `${GOOD}\nsecond\n`],
      ['spaced', `${GOOD} \n`],
      ['double-newline', `${GOOD}\n\n`],
      ['non-ascii', `${GOOD}é\n`],
      ['nul', `${GOOD}\0`],
      ['too-long', `ncryptsec1${'q'.repeat(300)}\n`],
    ] as const) {
      expect(readBackupFile(put(name, body)), name).toMatchObject({ ok: false, reason: 'not_backup' })
    }
    expect(readBackupFile(put('empty', ''))).toMatchObject({ ok: false, reason: 'bad_size' })
    expect(readBackupFile(put('big', `ncryptsec1${'q'.repeat(5000)}`))).toMatchObject({ ok: false, reason: 'bad_size' })
  })

  it('refuses a symlink, a directory, a FIFO and a missing file', () => {
    const real = put('real', `${GOOD}\n`)
    const link = path.join(dir, 'link')
    symlinkSync(real, link)
    expect(readBackupFile(link)).toMatchObject({ ok: false })
    const sub = path.join(dir, 'sub')
    mkdirSync(sub)
    expect(readBackupFile(sub)).toMatchObject({ ok: false })
    const fifo = path.join(dir, 'fifo')
    execFileSync('mkfifo', [fifo])
    expect(readBackupFile(fifo)).toMatchObject({ ok: false, reason: 'not_regular' })
    expect(readBackupFile(path.join(dir, 'missing'))).toMatchObject({ ok: false, reason: 'unreadable' })
  })

  it('a later change to the file cannot reach a backup that was already read', () => {
    const p = put('swap', `${GOOD}\n`)
    const first = readBackupFile(p)
    writeFileSync(p, `ncryptsec1${'p'.repeat(150)}\n`)
    expect(first).toEqual({ ok: true, ncryptsec: GOOD })
  })

  it('parseBackupText strips at most the one trailing line break', () => {
    expect(parseBackupText(`${GOOD}\n`)).toBe(GOOD)
    expect(parseBackupText(`${GOOD}\n\n`)).toBeNull()
  })
})
