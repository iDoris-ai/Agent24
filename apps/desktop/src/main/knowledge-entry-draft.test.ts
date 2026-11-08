import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  ENTRY_DRAFT_FORMAT,
  ENTRY_DRAFT_ID,
  ENTRY_DRAFT_VERSION,
  MAX_ENTRY_BODY,
  MAX_ENTRY_DRAFTS,
  MAX_ENTRY_TITLE,
  createEntryDraftStore,
  entryHash,
  type EntryDraftInput,
  type EntryKind,
  type EntryFloor,
} from './knowledge-entry-draft'

const FIXED_HASH = '71324bc83e7f9673612daccca1fb17eb51f27d144e7a920a773021542ddb52da'

function clock(start = Date.UTC(2026, 0, 1)): () => Date {
  let t = start
  return () => {
    const d = new Date(t)
    t += 1000
    return d
  }
}

function draftInput(patch: object = {}): EntryDraftInput {
  const record = patch as Record<string, unknown>
  const sourcePatch = (record.source ?? {}) as Record<string, unknown>
  const rest = { ...record }
  delete rest.source
  return {
    binding: 'binding-1',
    floor: 'ordinary',
    skill_version: 'sop-1',
    redaction_rules_version: 1,
    kind: 'sop',
    title: '开工检查',
    body: '先停机，再挂牌。',
    ...rest,
    source: {
      conversation_id: 'chat_demo',
      message_from: 0,
      message_to: 2,
      ...sourcePatch,
    },
  } as EntryDraftInput
}

let dir: string

beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'knowledge-entry-draft-'))
})

afterEach(() => {
  vi.restoreAllMocks()
  fs.rmSync(dir, { recursive: true, force: true })
})

// The fixed samples of CONTRACT-OPS-v2 W8.13 (#1668), read from the contract's own fixture.
const W813 = JSON.parse(fs.readFileSync(
  path.join(__dirname, '..', '..', '..', '..', 'protocol', 'knowledge', 'fixtures', 'ops-v2', 'entry-hash.samples.json'), 'utf8',
)) as { scheme: number; samples: Array<{ kind: 'sop' | 'faq' | 'note'; title: string; body: string; sha256: string }> }
const W813_SAMPLES = W813.samples

describe('entryHash (W8.13 scheme 1)', () => {
  it('recomputes every contract sample', () => {
    expect(W813.scheme).toBe(1)
    expect(W813_SAMPLES.length).toBeGreaterThanOrEqual(4)
    for (const sample of W813_SAMPLES) expect(entryHash(sample), sample.title).toBe(sample.sha256)
  })

  it('hashes the text exactly as given: the server normalizes nothing', () => {
    expect(entryHash({ kind: 'note', title: 't', body: 'a\r\nb' })).not.toBe(entryHash({ kind: 'note', title: 't', body: 'a\nb' }))
    expect(entryHash({ kind: 'note', title: 'e\u0301', body: 'b' })).not.toBe(entryHash({ kind: 'note', title: '\u00e9', body: 'b' }))
  })

  it('changes when kind, title, or body changes', () => {
    const base = entryHash({ kind: 'note', title: 'hi', body: 'x' })
    expect(entryHash({ kind: 'sop', title: 'hi', body: 'x' })).not.toBe(base)
    expect(entryHash({ kind: 'faq', title: 'hi', body: 'x' })).not.toBe(base)
    expect(entryHash({ kind: 'note', title: 'hi2', body: 'x' })).not.toBe(base)
    expect(entryHash({ kind: 'note', title: 'hi', body: 'y' })).not.toBe(base)
  })

  it('locks the hash of the fixed sample', () => {
    expect(entryHash({ kind: 'sop', title: '开工检查', body: '先停机，再挂牌。' })).toBe(FIXED_HASH)
  })
})

describe('canonical text (W8.13)', () => {
  it('is saved in canonical form and hashed exactly as stored', () => {
    const store = createEntryDraftStore(dir, clock())
    const draft = store.create({ ...draftInput(), title: ' \n e\u0301 \r\n', body: 'a\r\nb\rc\n' })
    expect(draft.title).toBe('\u00e9')
    expect(draft.body).toBe('a\nb\nc\n')
    expect(draft.content_sha256).toBe(entryHash({ kind: draft.kind, title: '\u00e9', body: 'a\nb\nc\n' }))
    expect(store.get(draft.id)).toEqual(draft)
    const updated = store.update(draft.id, { body: 'x\r\ny' })
    expect(updated.body).toBe('x\ny')
    expect(updated.content_sha256).toBe(entryHash({ kind: draft.kind, title: '\u00e9', body: 'x\ny' }))
  })

  it('refuses a lone surrogate in the title or the body', () => {
    const store = createEntryDraftStore(dir, clock())
    expect(() => store.create({ ...draftInput(), title: 'x\uD800' })).toThrow()
    expect(() => store.create({ ...draftInput(), body: '\uDC00y' })).toThrow()
    const draft = store.create(draftInput())
    expect(() => store.update(draft.id, { body: 'a\uD83D' })).toThrow()
    // A real pair is fine.
    expect(store.update(draft.id, { body: '🌿' }).body).toBe('🌿')
  })

  it('reads a stored draft whose text is not canonical as missing, even with a matching hash', () => {
    const store = createEntryDraftStore(dir, clock())
    const draft = store.create(draftInput())
    const file = path.join(dir, 'knowledge-entries', `${draft.id}.json`)
    const raw = JSON.parse(fs.readFileSync(file, 'utf8'))
    for (const body of ['a\r\nb', 'e\u0301']) {
      fs.writeFileSync(file, JSON.stringify({ ...raw, body, content_sha256: entryHash({ kind: raw.kind, title: raw.title, body }) }))
      expect(store.get(draft.id), JSON.stringify(body)).toBeNull()
    }
  })
})

describe('entry draft store', () => {
  it('reads a created draft back from get and list, with 0o600 files and a 0o700 directory', () => {
    const store = createEntryDraftStore(dir, clock())
    const draft = store.create(draftInput())
    expect(draft.format).toBe(ENTRY_DRAFT_FORMAT)
    expect(draft.version).toBe(ENTRY_DRAFT_VERSION)
    expect(draft.id).toMatch(ENTRY_DRAFT_ID)
    expect(draft.content_sha256).toBe(FIXED_HASH)
    expect(draft.created_at).toBe(draft.updated_at)
    expect(store.get(draft.id)).toEqual(draft)
    expect(store.list()).toEqual([draft])
    const root = path.join(dir, 'knowledge-entries')
    expect(fs.statSync(root).mode & 0o777).toBe(0o700)
    expect(fs.statSync(path.join(root, `${draft.id}.json`)).mode & 0o777).toBe(0o600)
  })

  it('lists drafts by updated_at descending', () => {
    const store = createEntryDraftStore(dir, clock())
    const first = store.create(draftInput({ title: '一' }))
    const second = store.create(draftInput({ title: '二' }))
    const third = store.create(draftInput({ title: '三' }))
    expect(store.list().map(item => item.id)).toEqual([third.id, second.id, first.id])
  })

  it('updates kind, title, and body, refreshes the hash and updated_at, and keeps created_at', () => {
    const store = createEntryDraftStore(dir, clock())
    const draft = store.create(draftInput())
    const next = store.update(draft.id, { kind: 'faq', title: '  新标题  ', body: '新正文' })
    expect(next.created_at).toBe(draft.created_at)
    expect(next.updated_at).not.toBe(draft.updated_at)
    expect(next.content_sha256).not.toBe(draft.content_sha256)
    expect(next.content_sha256).toBe(entryHash({ kind: 'faq', title: '新标题', body: '新正文' }))
    expect(next.kind).toBe('faq')
    // Saved in canonical form (W8.13): the title trimmed.
    expect(next.title).toBe('新标题')
    expect(next.body).toBe('新正文')
    expect(next.binding).toBe(draft.binding)
    expect(next.source).toEqual(draft.source)
    expect(store.get(draft.id)).toEqual(next)
  })

  it('throws when update is given an id that is not on disk', () => {
    const store = createEntryDraftStore(dir)
    expect(() => store.update(`ked_${'b'.repeat(22)}`, { title: 'x' })).toThrow(Error)
  })

  it.each([
    ['kind', { kind: 'wiki' as EntryKind }],
    ['empty title', { title: '   ' }],
    ['long title', { title: 'a'.repeat(MAX_ENTRY_TITLE + 1) }],
    ['long body', { body: 'a'.repeat(MAX_ENTRY_BODY + 1) }],
    ['empty binding', { binding: '' }],
    ['long binding', { binding: 'a'.repeat(2049) }],
    ['non-string binding', { binding: 1 as unknown as string }],
    ['conversation id', { source: { conversation_id: 'chat_' } }],
    ['reversed message range', { source: { message_from: 2, message_to: 1 } }],
    ['message_to past the limit', { source: { message_from: 0, message_to: 100001 } }],
    ['negative message_from', { source: { message_from: -1, message_to: 0 } }],
    ['fractional message_from', { source: { message_from: 1.5, message_to: 2 } }],
    ['string message index', { source: { message_from: '0' as unknown as number, message_to: 1 } }],
    ['floor', { floor: 'public' as EntryFloor }],
    ['skill version', { skill_version: 'Bad' }],
    ['empty skill version', { skill_version: '' }],
    ['redaction version 0', { redaction_rules_version: 0 }],
    ['fractional redaction version', { redaction_rules_version: 1.5 }],
  ])('create rejects %s', (_name, patch) => {
    const store = createEntryDraftStore(dir)
    expect(() => store.create(draftInput(patch))).toThrow(Error)
    expect(() => store.create(draftInput(patch))).toThrow(/^Invalid entry draft$/)
  })

  it.each([
    ['kind', { kind: 'wiki' }],
    ['empty title', { title: '  ' }],
    ['long title', { title: 'a'.repeat(MAX_ENTRY_TITLE + 1) }],
    ['long body', { body: 'a'.repeat(MAX_ENTRY_BODY + 1) }],
  ])('update rejects %s', (_name, patch) => {
    const store = createEntryDraftStore(dir)
    const draft = store.create(draftInput())
    expect(() => store.update(draft.id, patch)).toThrow(/^Invalid entry draft$/)
    expect(store.get(draft.id)?.title).toBe(draft.title)
  })

  it('accepts every kind, every floor, and the length boundaries', () => {
    const store = createEntryDraftStore(dir)
    const kinds: EntryKind[] = ['sop', 'faq', 'note']
    const floors: EntryFloor[] = ['ordinary', 'restricted', 'unknown', null]
    for (const kind of kinds) {
      for (const floor of floors) {
        expect(store.create(draftInput({ kind, floor, title: `${kind}-${String(floor)}` })).kind).toBe(kind)
      }
    }
    const edge = store.create(draftInput({
      binding: 'b'.repeat(2048),
      title: 't'.repeat(MAX_ENTRY_TITLE),
      body: '',
      skill_version: `a${'b'.repeat(63)}`,
      redaction_rules_version: 2,
      source: { conversation_id: `chat_${'a'.repeat(80)}`, message_from: 0, message_to: 100000 },
    }))
    expect(edge.body).toBe('')
    expect(edge.title).toHaveLength(MAX_ENTRY_TITLE)
    expect(store.create(draftInput({ title: 'x', body: 'y'.repeat(MAX_ENTRY_BODY) })).body).toHaveLength(MAX_ENTRY_BODY)
  })

  it('refuses create once MAX_ENTRY_DRAFTS drafts exist', () => {
    const store = createEntryDraftStore(dir)
    for (let i = 0; i < MAX_ENTRY_DRAFTS; i += 1) store.create(draftInput({ title: `t${i}` }))
    const root = path.join(dir, 'knowledge-entries')
    const before = fs.readdirSync(root).length
    expect(() => store.create(draftInput({ title: 'overflow' }))).toThrow(/Too many entry drafts/)
    expect(fs.readdirSync(root).length).toBe(before)
    expect(store.list()).toHaveLength(MAX_ENTRY_DRAFTS)
  })

  it('treats a hash mismatch, an id mismatch, broken JSON, and an illegal file name as missing', () => {
    const store = createEntryDraftStore(dir)
    const draft = store.create(draftInput({ title: '独特标题XYZ', body: '独特正文XYZ' }))
    const root = path.join(dir, 'knowledge-entries')
    const file = path.join(root, `${draft.id}.json`)
    const saved = JSON.parse(fs.readFileSync(file, 'utf8')) as { body: string; id: string }

    saved.body = 'tampered body'
    fs.writeFileSync(file, JSON.stringify(saved))
    expect(store.get(draft.id)).toBeNull()
    expect(store.list()).toEqual([])
    expect(fs.existsSync(file)).toBe(true)

    saved.body = '独特正文XYZ'
    saved.id = `ked_${'c'.repeat(22)}`
    fs.writeFileSync(file, JSON.stringify(saved))
    expect(store.get(draft.id)).toBeNull()
    expect(store.get(saved.id)).toBeNull()
    expect(store.list()).toEqual([])

    fs.writeFileSync(file, '{')
    expect(store.get(draft.id)).toBeNull()
    expect(store.list()).toEqual([])

    fs.writeFileSync(path.join(root, 'not-an-id.json'), JSON.stringify({ id: 'not-an-id' }))
    fs.writeFileSync(path.join(root, 'ked_SHORT.json'), '{}')
    expect(store.list()).toEqual([])
    expect(store.get('not-an-id')).toBeNull()
  })

  it('withdraws the file, appends one event line, and does not append again', () => {
    const store = createEntryDraftStore(dir, clock())
    const draft = store.create(draftInput({ title: '独特标题XYZ', body: '独特正文XYZ' }))
    const root = path.join(dir, 'knowledge-entries')
    const events = path.join(root, '_events.jsonl')
    expect(store.withdraw(`ked_${'a'.repeat(22)}`)).toBe(false)
    expect(fs.existsSync(events)).toBe(false)

    expect(store.withdraw(draft.id)).toBe(true)
    expect(fs.existsSync(path.join(root, `${draft.id}.json`))).toBe(false)
    expect(store.get(draft.id)).toBeNull()
    expect(() => store.update(draft.id, { title: 'gone' })).toThrow(/Entry draft not found/)
    expect(fs.statSync(events).mode & 0o777).toBe(0o600)
    const lines = fs.readFileSync(events, 'utf8').trim().split('\n')
    expect(lines).toHaveLength(1)
    const row = JSON.parse(lines[0]) as Record<string, unknown>
    expect(Object.keys(row)).toEqual(['at', 'id', 'event', 'content_sha256'])
    expect(row).toEqual({
      at: expect.stringMatching(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/),
      id: draft.id,
      event: 'withdrawn',
      content_sha256: draft.content_sha256,
    })
    expect(lines[0]).not.toContain('独特标题XYZ')
    expect(lines[0]).not.toContain('独特正文XYZ')

    const before = fs.readFileSync(events, 'utf8')
    expect(store.withdraw(draft.id)).toBe(false)
    expect(fs.readFileSync(events, 'utf8')).toBe(before)
  })

  it('keeps the draft when the withdrawal cannot be recorded', () => {
    const store = createEntryDraftStore(dir, clock())
    const draft = store.create(draftInput())
    const root = path.join(dir, 'knowledge-entries')
    // A directory where the event log should be: appending to it fails.
    fs.mkdirSync(path.join(root, '_events.jsonl'))
    expect(() => store.withdraw(draft.id)).toThrow()
    expect(store.get(draft.id)).toEqual(draft)
    expect(fs.readdirSync(root).filter((name) => name.endsWith('.tmp'))).toEqual([])
  })

  it('records a withdrawal once even when the file cannot be removed afterwards', () => {
    const store = createEntryDraftStore(dir, clock())
    const draft = store.create(draftInput())
    const root = path.join(dir, 'knowledge-entries')
    const rm = vi.spyOn(fs, 'rmSync').mockImplementation(() => { throw Object.assign(new Error('busy'), { code: 'EBUSY' }) })
    try {
      expect(store.withdraw(draft.id)).toBe(true)
      expect(store.withdraw(draft.id)).toBe(false)
    } finally {
      rm.mockRestore()
    }
    expect(store.get(draft.id)).toBeNull()
    expect(store.list()).toEqual([])
    expect(fs.readFileSync(path.join(root, '_events.jsonl'), 'utf8').trim().split('\n')).toHaveLength(1)
  })

  it('keeps the draft when it cannot be moved out of the store', () => {
    const store = createEntryDraftStore(dir, clock())
    const draft = store.create(draftInput())
    const root = path.join(dir, 'knowledge-entries')
    const rename = vi.spyOn(fs, 'renameSync').mockImplementationOnce(() => { throw Object.assign(new Error('busy'), { code: 'EBUSY' }) })
    try {
      expect(() => store.withdraw(draft.id)).toThrow('busy')
    } finally {
      rename.mockRestore()
    }
    expect(store.get(draft.id)).toEqual(draft)
    expect(fs.existsSync(path.join(root, '_events.jsonl'))).toBe(false)
  })

  it('does not touch a file outside the store when the id is illegal', () => {
    const store = createEntryDraftStore(dir)
    const sentinel = path.join(dir, 'x.json')
    fs.writeFileSync(sentinel, 'keep')
    const outside = path.join(path.dirname(dir), 'x.json')
    const outsideExisted = fs.existsSync(outside)
    expect(store.get('../x')).toBeNull()
    expect(store.get(null)).toBeNull()
    expect(store.get(1)).toBeNull()
    expect(() => store.update('../x', { title: 'nope' })).toThrow(/^Invalid entry draft$/)
    expect(() => store.update(null, { title: 'nope' })).toThrow(/^Invalid entry draft$/)
    expect(store.withdraw('../x')).toBe(false)
    expect(fs.readFileSync(sentinel, 'utf8')).toBe('keep')
    expect(fs.existsSync(outside)).toBe(outsideExisted)
    const root = path.join(dir, 'knowledge-entries')
    if (fs.existsSync(root)) {
      expect(fs.readdirSync(root).some(name => name.endsWith('.jsonl') || name.endsWith('.json'))).toBe(false)
    }
  })

  it('leaves no temp file when the write fails', () => {
    const store = createEntryDraftStore(dir)
    const write = fs.writeFileSync
    vi.spyOn(fs, 'writeFileSync').mockImplementation((target, data, options) => {
      if (typeof target === 'number') {
        write(target, String(data).slice(0, 8))
        throw new Error('ENOSPC')
      }
      return write(target, data, options)
    })
    expect(() => store.create(draftInput())).toThrow(/ENOSPC/)
    const root = path.join(dir, 'knowledge-entries')
    const names = fs.existsSync(root) ? fs.readdirSync(root) : []
    expect(names.filter(name => name.endsWith('.tmp'))).toEqual([])
    expect(names.filter(name => name.endsWith('.json'))).toEqual([])
  })

  it('drops temp files older than one hour and keeps younger ones', () => {
    const store = createEntryDraftStore(dir)
    store.create(draftInput())
    const root = path.join(dir, 'knowledge-entries')
    const oldTmp = path.join(root, `ked_${'a'.repeat(22)}.json.0123456789ab.tmp`)
    const youngTmp = path.join(root, `ked_${'b'.repeat(22)}.json.ba9876543210.tmp`)
    fs.writeFileSync(oldTmp, 'old')
    fs.writeFileSync(youngTmp, 'young')
    const hour = 60 * 60 * 1000
    const old = new Date(Date.now() - hour - 5000)
    const young = new Date(Date.now() - 30 * 60 * 1000)
    fs.utimesSync(oldTmp, old, old)
    fs.utimesSync(youngTmp, young, young)
    expect(store.list().length).toBe(1)
    expect(fs.existsSync(oldTmp)).toBe(false)
    expect(fs.existsSync(youngTmp)).toBe(true)
    expect(fs.readFileSync(youngTmp, 'utf8')).toBe('young')
  })
})

// PR-Daemon round 1 on #1677 (head 8d00bd36).
describe('entry draft store, PR-Daemon round 1', () => {
  it('N1: limits the body by its UTF-8 bytes, not its UTF-16 length', () => {
    const store = createEntryDraftStore(dir, clock())
    // 3 bytes each: 21 846 characters are 65 538 bytes.
    const over = '中'.repeat(Math.floor(MAX_ENTRY_BODY / 3) + 1)
    expect(over.length).toBeLessThan(MAX_ENTRY_BODY)
    expect(() => store.create(draftInput({ body: over }))).toThrow()
    const fits = '中'.repeat(Math.floor(MAX_ENTRY_BODY / 3))
    expect(store.create(draftInput({ body: fits })).body).toBe(fits)
    const draft = store.create(draftInput())
    expect(() => store.update(draft.id, { body: over })).toThrow()
  })

  it('N2: puts back a draft whose withdrawal was never recorded, and keeps it past the temp sweep', () => {
    const now = { t: Date.UTC(2026, 0, 1) }
    const store = createEntryDraftStore(dir, () => new Date(now.t))
    const draft = store.create(draftInput())
    const root = path.join(dir, 'knowledge-entries')
    // The app stopped after the move aside and before the event.
    fs.renameSync(path.join(root, `${draft.id}.json`), path.join(root, `${draft.id}.withdrawn.tmp`))
    const old = new Date(now.t - 2 * 60 * 60 * 1000)
    fs.utimesSync(path.join(root, `${draft.id}.withdrawn.tmp`), old, old)
    now.t += 2 * 60 * 60 * 1000
    const again = createEntryDraftStore(dir, () => new Date(now.t))
    expect(again.list()).toEqual([draft])
    expect(again.get(draft.id)).toEqual(draft)
    expect(fs.existsSync(path.join(root, `${draft.id}.withdrawn.tmp`))).toBe(false)
  })

  it('N2: an unrecorded aside that cannot go back is kept, not swept', () => {
    const now = { t: Date.UTC(2026, 0, 1) }
    const store = createEntryDraftStore(dir, () => new Date(now.t))
    const draft = store.create(draftInput())
    const root = path.join(dir, 'knowledge-entries')
    const aside = path.join(root, `${draft.id}.withdrawn.tmp`)
    fs.copyFileSync(path.join(root, `${draft.id}.json`), aside)
    const old = new Date(now.t - 2 * 60 * 60 * 1000)
    fs.utimesSync(aside, old, old)
    now.t += 2 * 60 * 60 * 1000
    expect(store.list()).toEqual([draft])
    expect(fs.existsSync(aside)).toBe(true)
  })

  it('N2: a recorded withdrawal whose file was left aside stays withdrawn and is swept', () => {
    const now = { t: Date.UTC(2026, 0, 1) }
    const store = createEntryDraftStore(dir, () => new Date(now.t))
    const draft = store.create(draftInput())
    const root = path.join(dir, 'knowledge-entries')
    const rm = vi.spyOn(fs, 'rmSync').mockImplementationOnce(() => { throw Object.assign(new Error('busy'), { code: 'EBUSY' }) })
    try {
      expect(store.withdraw(draft.id)).toBe(true)
    } finally {
      rm.mockRestore()
    }
    const aside = path.join(root, `${draft.id}.withdrawn.tmp`)
    expect(fs.existsSync(aside)).toBe(true)
    expect(store.list()).toEqual([])
    const old = new Date(now.t - 2 * 60 * 60 * 1000)
    fs.utimesSync(aside, old, old)
    now.t += 2 * 60 * 60 * 1000
    expect(store.list()).toEqual([])
    expect(fs.existsSync(aside)).toBe(false)
    expect(store.get(draft.id)).toBeNull()
  })

  it('N3: refuses to create when the store cannot be listed, rather than counting no drafts', () => {
    const store = createEntryDraftStore(dir, clock())
    store.create(draftInput())
    const readdir = vi.spyOn(fs, 'readdirSync').mockImplementation(() => { throw Object.assign(new Error('io'), { code: 'EIO' }) })
    try {
      expect(() => store.create(draftInput())).toThrow()
      expect(() => store.list()).toThrow()
    } finally {
      readdir.mockRestore()
    }
    expect(store.list()).toHaveLength(1)
  })

  it('N3: a saved draft is never reported as failed by a step after the rename', () => {
    const store = createEntryDraftStore(dir, clock())
    const chmod = vi.spyOn(fs, 'chmodSync')
    try {
      const draft = store.create(draftInput())
      const root = path.join(dir, 'knowledge-entries')
      const calls = chmod.mock.calls.map(([target]) => String(target))
      expect(calls.every((target) => target === root)).toBe(true)
      expect((fs.statSync(path.join(root, `${draft.id}.json`)).mode & 0o777)).toBe(0o600)
      expect(store.withdraw(draft.id)).toBe(true)
      expect(chmod.mock.calls.map(([target]) => String(target)).every((target) => target === root)).toBe(true)
      expect((fs.statSync(path.join(root, '_events.jsonl')).mode & 0o777)).toBe(0o600)
    } finally {
      chmod.mockRestore()
    }
  })
})

// Codex review of the round-1 fixes (8d00bd36..b3a9455c).
describe('entry draft store, Codex on round 1', () => {
  const asideOf = (root: string, id: string) => path.join(root, `${id}.withdrawn.tmp`)

  it('C1: an unreadable event log puts nothing back and sweeps nothing', () => {
    const now = { t: Date.UTC(2026, 0, 1) }
    const store = createEntryDraftStore(dir, () => new Date(now.t))
    const draft = store.create(draftInput())
    const root = path.join(dir, 'knowledge-entries')
    const rm = vi.spyOn(fs, 'rmSync').mockImplementationOnce(() => { throw Object.assign(new Error('busy'), { code: 'EBUSY' }) })
    try { expect(store.withdraw(draft.id)).toBe(true) } finally { rm.mockRestore() }
    const old = new Date(now.t - 2 * 60 * 60 * 1000)
    fs.utimesSync(asideOf(root, draft.id), old, old)
    now.t += 2 * 60 * 60 * 1000
    const events = path.join(root, '_events.jsonl')
    const real = fs.readFileSync
    const read = vi.spyOn(fs, 'readFileSync').mockImplementation(((file: fs.PathOrFileDescriptor, ...rest: unknown[]) => {
      if (file === events) throw Object.assign(new Error('io'), { code: 'EIO' })
      return (real as (...a: unknown[]) => unknown)(file, ...rest)
    }) as typeof fs.readFileSync)
    try {
      expect(store.list()).toEqual([])
      expect(store.get(draft.id)).toBeNull()
      expect(fs.existsSync(asideOf(root, draft.id))).toBe(true)
    } finally {
      read.mockRestore()
    }
    expect(store.list()).toEqual([])
    expect(fs.existsSync(asideOf(root, draft.id))).toBe(false)
  })

  it('C2: an event after a torn last line still counts', () => {
    const store = createEntryDraftStore(dir, clock())
    const a = store.create(draftInput({ title: 'A' }))
    const b = store.create(draftInput({ title: 'B' }))
    const root = path.join(dir, 'knowledge-entries')
    expect(store.withdraw(a.id)).toBe(true)
    // The app stopped mid-append once: a torn line with no line break.
    fs.appendFileSync(path.join(root, '_events.jsonl'), '{"at":"2026-01-01T00:00:00.000Z","id":"ked_')
    const rm = vi.spyOn(fs, 'rmSync').mockImplementationOnce(() => { throw Object.assign(new Error('busy'), { code: 'EBUSY' }) })
    try { expect(store.withdraw(b.id)).toBe(true) } finally { rm.mockRestore() }
    // B's file is still aside, and its event must be read as recorded.
    expect(store.list()).toEqual([])
    expect(store.get(b.id)).toBeNull()
    const lines = fs.readFileSync(path.join(root, '_events.jsonl'), 'utf8').split('\n')
    expect(lines.filter((l) => l.includes(b.id) && l.endsWith('}'))).toHaveLength(1)
  })

  it('C6: get, update and withdraw put back an unrecorded aside themselves', () => {
    for (const act of ['get', 'update', 'withdraw'] as const) {
      const store = createEntryDraftStore(dir, clock())
      const draft = store.create(draftInput())
      const root = path.join(dir, 'knowledge-entries')
      fs.renameSync(path.join(root, `${draft.id}.json`), asideOf(root, draft.id))
      const fresh = createEntryDraftStore(dir, clock())
      if (act === 'get') expect(fresh.get(draft.id)).toEqual(draft)
      if (act === 'update') expect(fresh.update(draft.id, { title: '改过' }).title).toBe('改过')
      if (act === 'withdraw') expect(fresh.withdraw(draft.id)).toBe(true)
      expect(fs.existsSync(asideOf(root, draft.id))).toBe(false)
      fs.rmSync(root, { recursive: true, force: true })
    }
  })

  it('C7: a close that fails after the event was written does not undo the withdrawal', () => {
    const store = createEntryDraftStore(dir, clock())
    const draft = store.create(draftInput())
    const realClose = fs.closeSync
    let failed = false
    const close = vi.spyOn(fs, 'closeSync').mockImplementation((fd: number) => {
      realClose(fd)
      if (!failed) {
        failed = true
        throw Object.assign(new Error('io'), { code: 'EIO' })
      }
    })
    try {
      expect(store.withdraw(draft.id)).toBe(true)
    } finally {
      close.mockRestore()
    }
    expect(failed).toBe(true)
    expect(store.get(draft.id)).toBeNull()
    expect(store.withdraw(draft.id)).toBe(false)
    const lines = fs.readFileSync(path.join(dir, 'knowledge-entries', '_events.jsonl'), 'utf8').trim().split('\n')
    expect(lines).toHaveLength(1)
  })
})

