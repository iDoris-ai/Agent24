// FU-17: the session map is the only record of which agent24 session belongs to
// which WeChat user. These tests pin the two properties that a plain
// `writeFileSync` does not have.
//
// MUTATION CHECK (how to confirm these tests are load-bearing):
//   1. Replace the temp+rename body of `save()` with a plain
//      `fs.writeFileSync(this.file, ...)`.
//      → "never leaves a half-written main file" and "recovers ... via the
//        backup" fail.
//   2. Make `rotateBackupIfValid()` copy unconditionally (drop the parse check).
//      → "never rotates a corrupt main file into the backup" fails.
//   3. Drop the shape checks from `parseMap` (keep only JSON.parse).
//      → "rejects on-disk shapes that are valid JSON but not a session map" fails.

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { FileSessionStore } from './session-store.js'

/** The backup's parsed contents — several tests assert on which generation it holds. */
function parseBak(): unknown {
  return JSON.parse(fs.readFileSync(`${file}.bak`, 'utf8'))
}

let dir: string
let file: string

beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-wechat-sessions-'))
  file = path.join(dir, 'nested', 'wechat-sessions.json')
})

afterEach(() => {
  fs.rmSync(dir, { recursive: true, force: true })
  vi.restoreAllMocks()
})

describe('FileSessionStore round-trip', () => {
  it('creates the directory and round-trips the map', () => {
    const store = new FileSessionStore(file)
    store.save(new Map([['uid-1', 'sess-a'], ['uid-2', 'sess-b']]))

    expect(new FileSessionStore(file).load()).toEqual(
      new Map([['uid-1', 'sess-a'], ['uid-2', 'sess-b']]),
    )
  })

  it('returns an empty map when nothing has been written yet', () => {
    expect(new FileSessionStore(file).load()).toEqual(new Map())
  })

  it('writes the file 0600 — it maps real users to their sessions', () => {
    new FileSessionStore(file).save(new Map([['uid-1', 'sess-a']]))
    expect(fs.statSync(file).mode & 0o777).toBe(0o600)
  })
})

describe('FileSessionStore crash safety', () => {
  it('never leaves a half-written main file: the write lands via rename', () => {
    const store = new FileSessionStore(file)
    store.save(new Map([['uid-1', 'sess-a']]))

    // Fail the second write at the point a truncating writer would already have
    // destroyed the main file. `rename` is what publishes, so the ORIGINAL
    // content must still be fully readable afterwards.
    const realWrite = fs.writeFileSync
    vi.spyOn(fs, 'writeFileSync').mockImplementationOnce(((p: fs.PathOrFileDescriptor, ...rest: unknown[]) => {
      // Simulate a crash partway through producing the new content.
      ;(realWrite as unknown as (...a: unknown[]) => void)(p, '{"uid-1": "sess-', ...rest.slice(1))
      throw new Error('simulated power cut mid-write')
    }) as typeof fs.writeFileSync)

    store.save(new Map([['uid-1', 'sess-NEW']]))

    // The main file was never touched — the aborted write only dirtied `.tmp`.
    expect(JSON.parse(fs.readFileSync(file, 'utf8'))).toEqual({ 'uid-1': 'sess-a' })
    expect(new FileSessionStore(file).load()).toEqual(new Map([['uid-1', 'sess-a']]))
  })

  it('recovers from a truncated main file via the backup', () => {
    const store = new FileSessionStore(file)
    store.save(new Map([['uid-1', 'sess-a']]))
    store.save(new Map([['uid-1', 'sess-a'], ['uid-2', 'sess-b']])) // now .bak exists

    fs.writeFileSync(file, '{"uid-1": "ses') // truncated, as a power cut leaves it

    // Falls through to `.bak`, which holds the previous good generation.
    expect(new FileSessionStore(file).load()).toEqual(new Map([['uid-1', 'sess-a']]))
  })

  it('does NOT trust a leftover .tmp — it is an unfinished write, not a generation', () => {
    const store = new FileSessionStore(file)
    store.save(new Map([['uid-1', 'good']]))
    store.save(new Map([['uid-1', 'good']])) // .bak now holds a good generation

    fs.writeFileSync(file, 'not json at all')
    // Whatever a dead writer left behind must not become the answer.
    fs.writeFileSync(`${file}.${process.pid}.tmp`, JSON.stringify({ 'uid-1': 'truncated?' }))

    expect(new FileSessionStore(file).load()).toEqual(new Map([['uid-1', 'good']]))
  })

  // CODEX REVIEW (High): rotating an unvalidated main into .bak destroys the
  // backup in exactly the scenario it exists for. This is the regression test.
  it('never rotates a corrupt main file into the backup', () => {
    const store = new FileSessionStore(file)
    store.save(new Map([['uid-1', 'good']]))
    store.save(new Map([['uid-1', 'good'], ['uid-2', 'also-good']]))

    // Main is damaged from outside; .bak still holds the previous generation.
    fs.writeFileSync(file, '{corrupt')
    expect(new FileSessionStore(file).load()).toEqual(new Map([['uid-1', 'good']]))

    // A save now must NOT copy the corrupt main over the good .bak...
    store.save(new Map([['uid-3', 'new']]))
    expect(parseBak()).toEqual({ 'uid-1': 'good' })

    // ...so damaging main again still leaves something to recover.
    fs.writeFileSync(file, '{corrupt again')
    expect(new FileSessionStore(file).load()).toEqual(new Map([['uid-1', 'good']]))
  })

  it('rejects on-disk shapes that are valid JSON but not a session map', () => {
    fs.mkdirSync(path.dirname(file), { recursive: true })
    for (const bad of ['null', '[1,2]', '"a string"', '42', '{"uid":123}']) {
      fs.writeFileSync(file, bad)
      expect(new FileSessionStore(file).load()).toEqual(new Map())
    }
  })

  it('returns an empty map rather than throwing when every candidate is corrupt', () => {
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.writeFileSync(file, '{trunc')
    fs.writeFileSync(`${file}.tmp`, '{also trunc')
    fs.writeFileSync(`${file}.bak`, '{and this')

    expect(new FileSessionStore(file).load()).toEqual(new Map())
  })
})
