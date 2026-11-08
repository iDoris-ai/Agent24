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

// The fixed samples of CONTRACT-OPS-v2 W8.13 (#1668,
// protocol/knowledge/fixtures/ops-v2/entry-hash.samples.json).
const W813_SAMPLES = [
  { kind: 'note', title: 'VPN setup', body: '1. Open the client\n2. Sign in', sha256: 'dc2552272966f63f77d27dcceb7dd489ccf1129aa70fd803a654542ff679302c' },
  { kind: 'sop', title: '请假流程 🌿', body: '## 步骤\n\n1. 打开「人事」系统\n2. 提交申请 🙂\n\n> 审批人：直属主管', sha256: 'cd192d152a12cfe33ea1ae7687e0653d911e24971edfc4bca2146cf8bbfbf30f' },
  { kind: 'faq', title: '空正文也可以', body: '', sha256: '08edefbebcffd610f7fc1c349e256ff334693c582b681cc8ea49f1d31135a7f9' },
  { kind: 'note', title: '引号 " 与反斜杠 \\', body: '列1\t列2\n\u0001结尾', sha256: 'b5ab5a1bfb076d71cf540e369d8cbc30a4c13d9bb3ce442c03b6147aeedecf1e' },
] as const

describe('entryHash (W8.13 scheme 1)', () => {
  it('recomputes every contract sample', () => {
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
