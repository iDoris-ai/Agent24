// FU-17: the session map is the only record of which agent24 session belongs to
// which WeChat user. These tests pin the two properties that a plain
// `writeFileSync` does not have.
//
// MUTATION CHECK (how to confirm these tests are load-bearing):
//   In `session-store.ts` `save()`, replace the temp+rename body with
//   `fs.writeFileSync(this.file, JSON.stringify(...))`.
//   → "a reader never sees a half-written file" fails (a truncated file is
//     observable), and "recovers from a truncated main file" fails (no .bak).

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { FileSessionStore } from './session-store.js'

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

  it('prefers a leftover .tmp over an older .bak — .tmp is the newer content', () => {
    const store = new FileSessionStore(file)
    store.save(new Map([['uid-1', 'old']]))
    store.save(new Map([['uid-1', 'older-still']])) // .bak = {uid-1: old}

    fs.writeFileSync(file, 'not json at all')
    fs.writeFileSync(`${file}.tmp`, JSON.stringify({ 'uid-1': 'newest' }))

    expect(new FileSessionStore(file).load()).toEqual(new Map([['uid-1', 'newest']]))
  })

  it('returns an empty map rather than throwing when every candidate is corrupt', () => {
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.writeFileSync(file, '{trunc')
    fs.writeFileSync(`${file}.tmp`, '{also trunc')
    fs.writeFileSync(`${file}.bak`, '{and this')

    expect(new FileSessionStore(file).load()).toEqual(new Map())
  })
})
