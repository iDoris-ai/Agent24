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
//   4. Make `load()` return the recovered map without calling `republish`.
//      → "repairs the main file after recovering from the backup" fails.
//   5. Make `rotateBackupIfValid` use `copyFileSync` instead of temp+rename.
//      → "leaves the old backup intact when rotation fails" fails.
//   6. Delete the `sweepTemps()` call from `load()`.
//      → "sweeps temps orphaned by a crashed writer" fails.

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

    // Die partway through producing the new MAIN content — the moment at which
    // a truncating writer has already destroyed the file. `rename` is what
    // publishes, so the ORIGINAL content must still be fully readable after.
    //
    // A `save()` performs two durable writes: the backup rotation first, then
    // the main generation. Let the rotation through and fail the main write, or
    // the test would be exercising rotation instead of publication.
    const realWrite = fs.writeFileSync
    let writes = 0
    vi.spyOn(fs, 'writeFileSync').mockImplementation(((p: fs.PathOrFileDescriptor, ...rest: unknown[]) => {
      writes += 1
      const call = (...a: unknown[]): void => (realWrite as unknown as (...a: unknown[]) => void)(...a)
      if (writes < 2) return call(p, ...rest)
      call(p, '{"uid-1": "sess-', ...rest.slice(1)) // truncated content
      throw new Error('simulated power cut mid-write')
    }) as typeof fs.writeFileSync)

    store.save(new Map([['uid-1', 'sess-NEW']]))
    vi.restoreAllMocks()

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

  // CODEX REVIEW round 2 (Medium): falling back to .bak and then just carrying
  // on leaves the install running on a single copy — the next corruption loses
  // everything, and nothing warned anyone. Recovery must restore redundancy.
  it('repairs the main file after recovering from the backup', () => {
    const store = new FileSessionStore(file)
    store.save(new Map([['uid-1', 'a']]))
    store.save(new Map([['uid-1', 'a'], ['uid-2', 'b']])) // .bak = {uid-1:a}

    fs.writeFileSync(file, '{corrupt')
    expect(new FileSessionStore(file).load()).toEqual(new Map([['uid-1', 'a']]))

    // main is now a real generation again, not still-corrupt.
    expect(JSON.parse(fs.readFileSync(file, 'utf8'))).toEqual({ 'uid-1': 'a' })
    // ...so a later corruption is still survivable.
    fs.writeFileSync(file, '{corrupt again')
    expect(new FileSessionStore(file).load()).toEqual(new Map([['uid-1', 'a']]))
  })

  // CODEX REVIEW round 2 (Medium): copyFileSync truncates the destination, so a
  // failed rotation used to damage .bak AND let the save proceed — the same
  // defect class as round 1's High, one level down.
  it('leaves the old backup intact when rotation fails', () => {
    const store = new FileSessionStore(file)
    store.save(new Map([['uid-1', 'keep-me']]))
    store.save(new Map([['uid-1', 'keep-me'], ['uid-2', 'x']])) // .bak = {uid-1:keep-me}

    // Fail the rotation write (the first durable write of the next save).
    vi.spyOn(fs, 'writeFileSync').mockImplementationOnce(() => {
      throw new Error('simulated ENOSPC during backup rotation')
    })
    store.save(new Map([['uid-3', 'new']]))

    expect(parseBak()).toEqual({ 'uid-1': 'keep-me' })
  })

  // CODEX REVIEW round 2 (Low): SIGKILL bypasses the catch that removes a temp,
  // so without a sweep one orphan accumulates per crashed PID, forever.
  it('sweeps temps orphaned by a crashed writer, but not a live one', () => {
    const store = new FileSessionStore(file)
    store.save(new Map([['uid-1', 'a']]))

    const orphan = `${file}.999999.tmp`
    const bakOrphan = `${file}.bak.999998.tmp`
    const mine = `${file}.${process.pid}.tmp`
    const unrelated = path.join(path.dirname(file), 'something-else.tmp')
    for (const f of [orphan, bakOrphan, mine, unrelated]) fs.writeFileSync(f, '{}')

    new FileSessionStore(file).load()

    expect(fs.existsSync(orphan)).toBe(false)
    expect(fs.existsSync(bakOrphan)).toBe(false)
    expect(fs.existsSync(mine)).toBe(true) // a concurrent write of ours
    expect(fs.existsSync(unrelated)).toBe(true) // not ours to delete
  })
})
