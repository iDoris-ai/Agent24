// FU-17: the session map is the only record of which agent24 session belongs to
// which WeChat user. These tests pin the two properties that a plain
// `writeFileSync` does not have.
//
// MUTATION CHECK (how to confirm these tests are load-bearing):
//   1. Replace the temp+rename body of `save()` with a plain
//      `fs.writeFileSync(this.file, ...)`.
//      → "never leaves a half-written main file" and "recovers ... via the
//        backup" fail.
//   2. Make `readValidMain()` return the text without the `parseMap` check.
//      → "never rotates a corrupt main file into the backup" fails, because the
//        corrupt bytes reach `.bak` and `parseBak()` then throws. NOTE: that
//        test must NOT call `load()` between damaging main and saving — `load()`
//        repairs main, which silently disarmed this very check once
//        republish-on-recover landed.
//   3. Drop the shape checks from `parseMap` (keep only JSON.parse).
//      → "rejects on-disk shapes that are valid JSON but not a session map" fails.
//   4. Make `load()` return the recovered map without calling `republish`.
//      → "repairs the main file after recovering from the backup" fails.
//   5. Make `writeBackup` use `copyFileSync` instead of temp+rename.
//      → "leaves the old backup intact when the rotation write fails" fails.
//   6. Delete the `sweepTemps()` call from `load()`.
//      → "sweeps only temps old enough to be abandoned" fails.
//   7. Make `sweepTemps` treat a foreign PID as an orphan (drop the age check).
//      → the same test fails: it deletes another process's live temp.
//   8. In `save()`, rotate the backup BEFORE calling `republish`.
//      → "a failed publish does not consume the rollback copy" fails.

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
    // `save()` publishes the primary FIRST and only then rotates the generation
    // it replaced, so the primary write is the first durable write of the call.
    const realWrite = fs.writeFileSync
    vi.spyOn(fs, 'writeFileSync').mockImplementationOnce(((p: fs.PathOrFileDescriptor, ...rest: unknown[]) => {
      ;(realWrite as unknown as (...a: unknown[]) => void)(p, '{"uid-1": "sess-', ...rest.slice(1))
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

    // Save DIRECTLY, with no intervening `load()`. This is load-bearing: `load()`
    // repairs main on recovery, so calling it here would hand `save()` a valid
    // file and the guard under test would never be reached. That is exactly how
    // this test lost its discriminating power once republish-on-recover landed —
    // it passed with the guard removed. (Caught by PR-Daemon on #142.)
    store.save(new Map([['uid-3', 'new']]))

    // The new generation is published over the corrupt file...
    expect(new FileSessionStore(file).load()).toEqual(new Map([['uid-3', 'new']]))
    // ...and the corrupt bytes were NOT promoted into the rollback copy, which
    // still holds the last generation anyone could actually read.
    expect(parseBak()).toEqual({ 'uid-1': 'good' })
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
  it('leaves the old backup intact when the rotation write fails', () => {
    const store = new FileSessionStore(file)
    store.save(new Map([['uid-1', 'g1']]))
    store.save(new Map([['uid-1', 'g2']])) // .bak = {uid-1:g1}, main = {uid-1:g2}

    // Let the primary publish through, fail the backup write that follows it.
    const realWrite = fs.writeFileSync
    let writes = 0
    vi.spyOn(fs, 'writeFileSync').mockImplementation(((...a: unknown[]) => {
      writes += 1
      if (writes >= 2) throw new Error('simulated ENOSPC during backup rotation')
      return (realWrite as unknown as (...x: unknown[]) => void)(...a)
    }) as typeof fs.writeFileSync)
    store.save(new Map([['uid-1', 'g3']]))
    vi.restoreAllMocks()

    // The new generation is published; the fallback is simply one older.
    expect(JSON.parse(fs.readFileSync(file, 'utf8'))).toEqual({ 'uid-1': 'g3' })
    expect(parseBak()).toEqual({ 'uid-1': 'g1' })
  })

  // CODEX REVIEW round 3 (blocking, case B): rotating BEFORE publishing meant a
  // failed publish left .bak holding the same generation as main — rollback
  // depth zero, silently, as the cost of a save that did not even happen.
  it('a failed publish does not consume the rollback copy', () => {
    const store = new FileSessionStore(file)
    store.save(new Map([['uid-1', 'g1']]))
    store.save(new Map([['uid-1', 'g2']])) // .bak = g1, main = g2

    vi.spyOn(fs, 'writeFileSync').mockImplementationOnce(() => {
      throw new Error('simulated failure publishing the new generation')
    })
    store.save(new Map([['uid-1', 'g3']]))
    vi.restoreAllMocks()

    // Both files untouched: main is still g2 and the fallback is still g1.
    expect(JSON.parse(fs.readFileSync(file, 'utf8'))).toEqual({ 'uid-1': 'g2' })
    expect(parseBak()).toEqual({ 'uid-1': 'g1' })
  })

  // CODEX REVIEW round 2 (Low): SIGKILL bypasses the catch that removes a temp,
  // so without a sweep one orphan accumulates per crash, forever.
  //
  // CODEX REVIEW round 3 (BLOCKING): the first version used "a different PID
  // means an orphan", which let one process delete a live writer's in-flight
  // temp — a failure mode this branch would have INTRODUCED. Age is the test.
  it('sweeps only temps old enough to be abandoned, never a live write', () => {
    const store = new FileSessionStore(file)
    store.save(new Map([['uid-1', 'a']]))

    const stale = `${file}.999999.tmp`
    const staleBak = `${file}.bak.999998.tmp`
    const liveOther = `${file}.999997.tmp` // ANOTHER process, writing right now
    const unrelated = path.join(path.dirname(file), 'something-else.tmp')
    for (const f of [stale, staleBak, liveOther, unrelated]) fs.writeFileSync(f, '{}')

    // Backdate only the two that a crash would have left behind.
    const longAgo = new Date(Date.now() - 3 * 60 * 60 * 1000)
    for (const f of [stale, staleBak]) fs.utimesSync(f, longAgo, longAgo)

    new FileSessionStore(file).load()

    expect(fs.existsSync(stale)).toBe(false)
    expect(fs.existsSync(staleBak)).toBe(false)
    expect(fs.existsSync(liveOther)).toBe(true) // ← the round-3 regression
    expect(fs.existsSync(unrelated)).toBe(true) // not ours to delete
  })
})
