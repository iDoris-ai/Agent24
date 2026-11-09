import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createChatHistoryStore, onChatHistoryChange, parseChatHistoryRecord } from './chat-history'
import { CHAT_HISTORY_FORMAT, CHAT_HISTORY_VERSION, type ChatHistoryRecord } from '../shared/chat-history'

let dir: string
beforeEach(() => { dir = fs.mkdtempSync(path.join(os.tmpdir(), 'chat-history-')) })
afterEach(() => { vi.restoreAllMocks(); fs.rmSync(dir, { recursive: true, force: true }) })

function record(id: string, updated: string, title = id): ChatHistoryRecord {
  return {
    format: CHAT_HISTORY_FORMAT,
    version: CHAT_HISTORY_VERSION,
    id,
    title,
    title_source: 'prompt', group_id: null, archived_at: null, runs: [], pending: [],
    created_at: '2026-11-08T00:00:00.000Z',
    updated_at: updated,
    messages: [
      { role: 'user', content: '你好', at: '2026-11-08T00:00:00.000Z', delivered: true },
      { role: 'assistant', content: 'hi', at: '2026-11-08T00:00:01.000Z', run_id: null,
        latency: { modelName: 'm', tier: 'local', firstTokenMs: null, totalMs: 12 } },
      { role: 'user', content: 'lost', at: '2026-11-08T00:00:02.000Z', delivered: false },
    ],
  }
}

describe('chat history on disk', () => {
  it('saves, reads back, lists newest first and survives a new store instance', () => {
    const store = createChatHistoryStore(dir)
    expect(store.list()).toEqual([])
    store.save(record('chat_a', '2026-11-08T01:00:00.000Z'))
    store.save(record('chat_b', '2026-11-08T02:00:00.000Z'))
    const again = createChatHistoryStore(dir)
    expect(again.list().map(s => s.id)).toEqual(['chat_b', 'chat_a'])
    expect(again.get('chat_a')).toEqual(record('chat_a', '2026-11-08T01:00:00.000Z'))
    expect(fs.readdirSync(path.join(dir, 'chat-history')).sort()).toEqual(['chat_a.json', 'chat_b.json'])
  })

  it('writes a self-describing file: format, version and a time on every message', () => {
    createChatHistoryStore(dir).save(record('chat_a', '2026-11-08T01:00:00.000Z'))
    const raw = JSON.parse(fs.readFileSync(path.join(dir, 'chat-history', 'chat_a.json'), 'utf8'))
    expect(raw.format).toBe('agent24.desktop.chat-history')
    expect(raw.version).toBe(2)
    expect(raw.messages.every((m: { at?: string }) => typeof m.at === 'string')).toBe(true)
  })

  it('overwrites a conversation in place, deletes one, and clears only its own files', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', '2026-11-08T01:00:00.000Z'))
    store.save(record('chat_a', '2026-11-08T03:00:00.000Z', 'renamed'))
    store.save(record('chat_b', '2026-11-08T02:00:00.000Z'))
    expect(store.list()).toEqual([
      { id: 'chat_a', title: 'renamed', title_source: 'prompt', group_id: null, archived_at: null, created_at: '2026-11-08T00:00:00.000Z', updated_at: '2026-11-08T03:00:00.000Z', first_prompt: '你好', tasks: [], pending: [] },
      { id: 'chat_b', title: 'chat_b', title_source: 'prompt', group_id: null, archived_at: null, created_at: '2026-11-08T00:00:00.000Z', updated_at: '2026-11-08T02:00:00.000Z', first_prompt: '你好', tasks: [], pending: [] },
    ])
    store.delete('chat_a')
    expect(store.get('chat_a')).toBeNull()
    store.delete('chat_a')
    fs.writeFileSync(path.join(dir, 'chat-history', 'notes.txt'), 'keep')
    store.clear()
    expect(store.list()).toEqual([])
    expect(fs.readdirSync(path.join(dir, 'chat-history'))).toEqual(['notes.txt'])
  })

  it.each(['../evil', 'chat_A', 'chat_', 'x', 'chat_a/../b', 3, null])('refuses id %s', id => {
    const store = createChatHistoryStore(dir)
    expect(() => store.get(id)).toThrow('Invalid chat history id')
    expect(() => store.delete(id)).toThrow('Invalid chat history id')
  })

  it.each([
    ['another version', { version: 3 }],
    ['version 1', { version: 1 }],
    ['no title source', { title_source: undefined }],
    ['no group', { group_id: undefined }],
    ['no archive mark', { archived_at: undefined }],
    ['no runs', { runs: undefined }],
    ['no pending', { pending: undefined }],
    ['another format', { format: 'x' }],
    ['a bad id', { id: '../x' }],
    ['a bad time', { updated_at: 'yesterday' }],
    ['a local time without zone', { updated_at: '2026-11-08T09:00:00' }],
    ['a time with an offset', { updated_at: '2026-11-08T09:00:00+08:00' }],
    ['a bad message', { messages: [{ role: 'system', content: 'x', at: '2026-11-08T00:00:00.000Z' }] }],
    ['an unknown title source', { title_source: 'robot' }],
    ['a null title source', { title_source: null }],
    ['a user message without delivery', { messages: [{ role: 'user', content: 'x', at: '2026-11-08T00:00:00.000Z' }] }],
  ])('refuses a record with %s and keeps the previous file', (_name, patch) => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', '2026-11-08T01:00:00.000Z'))
    expect(() => store.save({ ...record('chat_a', '2026-11-08T05:00:00.000Z'), ...patch })).toThrow()
    expect(store.get('chat_a')?.updated_at).toBe('2026-11-08T01:00:00.000Z')
  })

  it('M4 A2: does not read a version 1 file, nor migrate it (there is nothing of it to keep)', () => {
    const v1 = { ...record('chat_a', '2026-11-08T01:00:00.000Z', '旧标题'), version: 1 } as Partial<ChatHistoryRecord>
    delete v1.runs
    delete v1.pending
    fs.mkdirSync(path.join(dir, 'chat-history'))
    const file = path.join(dir, 'chat-history', 'chat_a.json')
    fs.writeFileSync(file, JSON.stringify(v1))
    const store = createChatHistoryStore(dir)
    expect(store.get('chat_a')).toBeNull()
    expect(store.list()).toEqual([])
    expect(JSON.parse(fs.readFileSync(file, 'utf8')).version).toBe(1)
  })

  it('drops unknown fields instead of storing them', () => {
    const parsed = parseChatHistoryRecord({ ...record('chat_a', '2026-11-08T01:00:00.000Z'), extra: 'x' })
    expect(parsed).not.toHaveProperty('extra')
  })

  it('refuses a sparse message list and keeps the previous conversation', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', '2026-11-08T01:00:00.000Z'))
    const sparse = record('chat_a', '2026-11-08T02:00:00.000Z')
    sparse.messages = new Array(3)
    sparse.messages[0] = { role: 'user', content: 'x', at: '2026-11-08T00:00:00.000Z', delivered: true }
    expect(() => store.save(sparse)).toThrow('Invalid chat history record')
    expect(store.get('chat_a')?.updated_at).toBe('2026-11-08T01:00:00.000Z')
    expect(store.list().map(s => s.id)).toEqual(['chat_a'])
  })

  it('refuses too many messages before parsing them, but keeps a large record that fits', () => {
    const store = createChatHistoryStore(dir)
    const big = record('chat_a', '2026-11-08T01:00:00.000Z')
    big.messages = Array.from({ length: 160_000 }, () => ({ role: 'user' as const, content: '', at: '2026-11-08T00:00:00.000Z', delivered: true }))
    const stringify = vi.spyOn(JSON, 'stringify')
    expect(() => store.save(big)).toThrow('too large')
    expect(stringify).not.toHaveBeenCalled()
    stringify.mockRestore()
    // 100,001 empty messages write to about 7.8 MB: under the limit, so saved.
    const fits = record('chat_b', '2026-11-08T01:00:00.000Z')
    fits.messages = Array.from({ length: 100_001 }, () => ({ role: 'user' as const, content: '', at: '2026-11-08T00:00:00.000Z', delivered: true }))
    store.save(fits)
    expect(store.get('chat_b')?.messages).toHaveLength(100_001)
  })

  it('counts the title and other strings before parsing', () => {
    const big = record('chat_a', '2026-11-08T01:00:00.000Z')
    big.title = 'x'.repeat(9 * 1024 * 1024)
    big.messages = []
    const parse = vi.spyOn(JSON, 'stringify')
    expect(() => createChatHistoryStore(dir).save(big)).toThrow('too large')
    expect(parse).not.toHaveBeenCalled()
  })

  it('refuses an oversized record', () => {
    const big = record('chat_a', '2026-11-08T01:00:00.000Z')
    big.messages = [{ role: 'user', content: 'x'.repeat(9 * 1024 * 1024), at: '2026-11-08T00:00:00.000Z', delivered: true }]
    expect(() => createChatHistoryStore(dir).save(big)).toThrow('too large')
    expect(fs.existsSync(path.join(dir, 'chat-history', 'chat_a.json'))).toBe(false)
  })

  it('writes through a fresh temporary file, never through one already there', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', '2026-11-08T01:00:00.000Z'))
    const outside = path.join(dir, 'outside.txt')
    fs.writeFileSync(outside, 'keep me')
    // A planted link at the old fixed temporary name is neither followed nor removed.
    fs.symlinkSync(outside, path.join(dir, 'chat-history', 'chat_a.json.tmp'))
    store.save(record('chat_a', '2026-11-08T02:00:00.000Z'))
    expect(fs.readFileSync(outside, 'utf8')).toBe('keep me')
    expect(store.get('chat_a')?.updated_at).toBe('2026-11-08T02:00:00.000Z')
    const mode = fs.statSync(path.join(dir, 'chat-history', 'chat_a.json')).mode & 0o777
    expect(mode).toBe(0o600)
    expect(fs.readdirSync(path.join(dir, 'chat-history')).sort()).toEqual(['chat_a.json', 'chat_a.json.tmp'])
  })

  it('leaves no partial temporary file when the write fails, and keeps the previous conversation', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', '2026-11-08T01:00:00.000Z'))
    const write = fs.writeFileSync
    vi.spyOn(fs, 'writeFileSync').mockImplementation((target, data, options) => {
      if (typeof target === 'number') {
        write(target, String(data).slice(0, 10))
        throw new Error('ENOSPC: no space left on device')
      }
      return write(target, data, options)
    })
    expect(() => store.save(record('chat_a', '2026-11-08T02:00:00.000Z'))).toThrow('ENOSPC')
    expect(fs.readdirSync(path.join(dir, 'chat-history'))).toEqual(['chat_a.json'])
    expect(store.get('chat_a')?.updated_at).toBe('2026-11-08T01:00:00.000Z')
  })

  it('removes the temporary files of interrupted saves on delete and clear', () => {
    const store = createChatHistoryStore(dir)
    const root = path.join(dir, 'chat-history')
    store.save(record('chat_a', '2026-11-08T01:00:00.000Z'))
    store.save(record('chat_b', '2026-11-08T01:00:00.000Z'))
    fs.writeFileSync(path.join(root, 'chat_a.json.0123456789ab.tmp'), 'transcript a')
    fs.writeFileSync(path.join(root, 'chat_b.json.ba9876543210.tmp'), 'transcript b')
    fs.writeFileSync(path.join(root, 'chat_c.json.aaaaaaaaaaaa.tmp'), 'transcript c')
    store.delete('chat_a')
    expect(fs.readdirSync(root).sort()).toEqual(['chat_b.json', 'chat_b.json.ba9876543210.tmp', 'chat_c.json.aaaaaaaaaaaa.tmp'])
    fs.writeFileSync(path.join(root, 'notes.tmp'), 'keep')
    store.clear()
    expect(fs.readdirSync(root)).toEqual(['notes.tmp'])
  })

  it('skips damaged or mismatched files when listing and reading', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', '2026-11-08T01:00:00.000Z'))
    fs.writeFileSync(path.join(dir, 'chat-history', 'chat_b.json'), 'broken')
    fs.writeFileSync(path.join(dir, 'chat-history', 'chat_c.json'), JSON.stringify(record('chat_a', '2026-11-08T09:00:00.000Z')))
    expect(store.list().map(s => s.id)).toEqual(['chat_a'])
    expect(store.get('chat_b')).toBeNull()
    expect(store.get('chat_c')).toBeNull()
  })
})

describe('chat history titles (UI-04)', () => {
  const at = '2026-11-08T01:00:00.000Z'
  const later = '2026-11-08T02:00:00.000Z'
  const onDisk = (id: string) => JSON.parse(fs.readFileSync(path.join(dir, 'chat-history', `${id}.json`), 'utf8'))

  it('a save returns the summary as written', () => {
    expect(createChatHistoryStore(dir).save(record('chat_a', at, '问'))).toEqual(
      { id: 'chat_a', title: '问', title_source: 'prompt', group_id: null, archived_at: null, created_at: '2026-11-08T00:00:00.000Z', updated_at: at, first_prompt: '你好', tasks: [], pending: [] })
  })

  it('a rename is kept over every later save of the first prompt\'s title', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at, '首问'))
    expect(store.setTitle('chat_a', ' 周报\n草稿 ', 'user')).toMatchObject({ title: '周报 草稿', title_source: 'user', updated_at: at })
    // A later save, or a late retry of an older one, carries the first prompt.
    const summary = store.save(record('chat_a', later, '首问'))
    expect(summary).toMatchObject({ title: '周报 草稿', title_source: 'user', updated_at: later })
    expect(onDisk('chat_a')).toMatchObject({ title: '周报 草稿', title_source: 'user', updated_at: later })
    expect(store.get('chat_a')?.messages).toHaveLength(3)
    // Renaming again replaces the rename.
    expect(store.setTitle('chat_a', '又改', 'user')).toMatchObject({ title: '又改', title_source: 'user' })
  })

  it('the model\'s title replaces the first prompt\'s, and is kept over it, but never over a rename', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at, '首问'))
    expect(store.setTitle('chat_a', '模型标题', 'model')).toMatchObject({ title: '模型标题', title_source: 'model' })
    expect(store.save(record('chat_a', later, '首问'))).toMatchObject({ title: '模型标题', title_source: 'model' })
    store.setTitle('chat_a', '我的', 'user')
    // A model title arriving after the rename is refused; the rename stands.
    expect(store.setTitle('chat_a', '迟到的模型标题', 'model')).toMatchObject({ title: '我的', title_source: 'user' })
    expect(onDisk('chat_a')).toMatchObject({ title: '我的', title_source: 'user' })
  })

  it('a save at the same rank as the title on file keeps it, unless that is the prompt\'s (PR-Daemon #1640 L3)', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at, '首问'))
    store.setTitle('chat_a', '新的模型标题', 'model')
    expect(store.save({ ...record('chat_a', later, '旧的模型标题'), title_source: 'model' })).toMatchObject({ title: '新的模型标题' })
    store.setTitle('chat_a', '新名', 'user')
    expect(store.save({ ...record('chat_a', later, '旧名'), title_source: 'user' })).toMatchObject({ title: '新名', title_source: 'user' })
    const other = createChatHistoryStore(dir)
    other.save(record('chat_b', at, '首问'))
    expect(other.save(record('chat_b', later, '首问改了'))).toMatchObject({ title: '首问改了', title_source: 'prompt' })
  })

  it('a save with a stronger title replaces a weaker one on file', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at, '首问'))
    expect(store.save({ ...record('chat_a', later, '自带'), title_source: 'user' })).toMatchObject({ title: '自带', title_source: 'user' })
  })

  it('retitles only a conversation on file: never creates one', () => {
    const store = createChatHistoryStore(dir)
    expect(store.setTitle('chat_gone', '名', 'user')).toBeNull()
    expect(fs.existsSync(path.join(dir, 'chat-history', 'chat_gone.json'))).toBe(false)
    store.save(record('chat_a', at))
    store.delete('chat_a')
    expect(store.setTitle('chat_a', '名', 'model')).toBeNull()
    expect(store.list()).toEqual([])
  })

  it.each([
    ['a bad id', '../x', '名', 'user', 'Invalid chat history id'],
    ['an unknown source', 'chat_a', '名', 'robot', 'Invalid chat history title source'],
    ['the prompt as source', 'chat_a', '名', 'prompt', 'Invalid chat history title source'],
    ['an empty title', 'chat_a', ' \n ', 'user', 'Invalid chat history title'],
    ['a title that is not text', 'chat_a', 3, 'user', 'Invalid chat history title'],
    ['an overlong title', 'chat_a', 'x'.repeat(4001), 'user', 'Invalid chat history title'],
  ])('refuses a retitle with %s and keeps the title', (_name, id, title, source, error) => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at, '首问'))
    expect(() => store.setTitle(id, title, source)).toThrow(error)
    expect(store.get('chat_a')).toMatchObject({ title: '首问', title_source: 'prompt' })
  })

  it('takes a rename of 80 long emoji sequences', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at, '首问'))
    const flags = '🏴󠁧󠁢󠁳󠁣󠁴󠁿'.repeat(80) // 14 UTF-16 units each
    expect(store.setTitle('chat_a', flags, 'user')).toMatchObject({ title: flags })
  })

  it('a summary carries the first prompt on one line, cut to 200 characters (UI-04 T3)', () => {
    const store = createChatHistoryStore(dir)
    const long = record('chat_a', at)
    long.messages = [
      { role: 'assistant', content: '先说话的是助手', at, run_id: null, latency: null },
      { role: 'user', content: `  第一行\n\n${'字'.repeat(300)}`, at, delivered: true },
    ]
    const first = `第一行 ${'字'.repeat(196)}`
    expect(store.save(long).first_prompt).toBe(first)
    expect(store.list()[0].first_prompt).toBe(first)
    expect(store.setTitle('chat_a', '名', 'user')?.first_prompt).toBe(first)
    // A pasted prompt of megabytes still gives its first 200 characters.
    const huge = record('chat_c', at)
    huge.messages = [{ role: 'user', content: `开头${'字'.repeat(2_000_000)}`, at, delivered: true }]
    expect(store.save(huge).first_prompt).toBe(`开头${'字'.repeat(198)}`)
    // White space taking the start of a long prompt is not all there is.
    const spaced = record('chat_d', at)
    spaced.messages = [{ role: 'user', content: `${' '.repeat(10_000)}budget\n\n\tupdate${' '.repeat(10_000)}`, at, delivered: true }]
    expect(store.save(spaced).first_prompt).toBe('budget update')
    const none = record('chat_b', at)
    none.messages = []
    expect(store.save(none).first_prompt).toBe('')
  })

  it('a retitle writes through a fresh temporary file like a save', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at, '首问'))
    const open = vi.spyOn(fs, 'openSync')
    store.setTitle('chat_a', '名', 'user')
    expect(open).toHaveBeenCalledWith(expect.stringMatching(/chat_a\.json\.[0-9a-f]{12}\.tmp$/), 'wx', 0o600)
    expect(fs.readdirSync(path.join(dir, 'chat-history'))).toEqual(['chat_a.json'])
  })
})

describe('chat history groups and archive (UI-06)', () => {
  const at = '2026-11-08T01:00:00.000Z'
  const later = '2026-11-08T02:00:00.000Z'
  const onDisk = (id: string) => JSON.parse(fs.readFileSync(path.join(dir, 'chat-history', `${id}.json`), 'utf8'))

  it('lists a file without group_id or archived_at as no conversation: version 2 has them all', () => {
    const old: Partial<ChatHistoryRecord> = record('chat_a', at)
    delete old.group_id
    delete old.archived_at
    fs.mkdirSync(path.join(dir, 'chat-history'))
    fs.writeFileSync(path.join(dir, 'chat-history', 'chat_a.json'), JSON.stringify(old))
    expect(createChatHistoryStore(dir).get('chat_a')).toBeNull()
  })

  it.each([
    ['a bad group id', { group_id: '../x' }],
    ['a conversation id as group', { group_id: 'chat_a' }],
    ['a bad archive time', { archived_at: 'yesterday' }],
  ])('refuses a record with %s', (_name, patch) => {
    expect(() => parseChatHistoryRecord({ ...record('chat_a', at), ...patch })).toThrow('Invalid chat history record')
  })

  it('moves and archives through organize; saves never change either, however late', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at))
    const { groups: [work] } = store.createGroup(' 工作\n项目 ')
    expect(work).toMatchObject({ name: '工作 项目', collapsed: false, id: expect.stringMatching(/^grp_[a-z0-9_]+$/) })
    expect(store.organize('chat_a', { group_id: work.id })).toMatchObject({ group_id: work.id, archived_at: null, updated_at: at })
    const archived = store.organize('chat_a', { archived: true })!
    expect(archived.archived_at).toMatch(/Z$/)
    expect(archived).toMatchObject({ group_id: work.id, updated_at: at })
    // Archiving again keeps the first time.
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2027-01-01T00:00:00.000Z'))
    try { expect(store.organize('chat_a', { archived: true })?.archived_at).toBe(archived.archived_at) }
    finally { vi.useRealTimers() }
    // The page's save carries neither; a page that sent them is ignored too.
    const summary = store.save({ ...record('chat_a', later), group_id: null, archived_at: null })
    expect(summary).toMatchObject({ group_id: work.id, archived_at: archived.archived_at, updated_at: later })
    expect(onDisk('chat_a')).toMatchObject({ group_id: work.id, archived_at: archived.archived_at })
    const fresh = store.save({ ...record('chat_b', at), group_id: work.id, archived_at: at })
    expect(fresh).toMatchObject({ group_id: null, archived_at: null })
    // Restoring keeps the group, so it goes back where it was; null takes it out.
    expect(store.organize('chat_a', { archived: false })).toMatchObject({ group_id: work.id, archived_at: null })
    expect(store.organize('chat_a', { group_id: null })).toMatchObject({ group_id: null })
    expect(store.list().map(s => [s.id, s.group_id, s.archived_at])).toEqual([['chat_a', null, null], ['chat_b', null, null]])
  })

  it('organizes only a conversation on file, into a group that exists', () => {
    const store = createChatHistoryStore(dir)
    const { groups: [work] } = store.createGroup('工作')
    expect(store.organize('chat_gone', { archived: true })).toBeNull()
    expect(fs.existsSync(path.join(dir, 'chat-history', 'chat_gone.json'))).toBe(false)
    store.save(record('chat_a', at))
    expect(() => store.organize('chat_a', { group_id: 'grp_never' })).toThrow('Unknown chat group')
    store.deleteGroup(work.id)
    expect(() => store.organize('chat_a', { group_id: work.id })).toThrow('Unknown chat group')
    expect(store.get('chat_a')).toMatchObject({ group_id: null })
  })

  it.each([
    ['no change', {}],
    ['an unknown field', { title: 'x' }],
    ['an unknown field beside a known one', { archived: true, title: 'x' }],
    ['a non-boolean archive', { archived: 'yes' }],
    ['a bad group id', { group_id: '../x' }],
    ['not an object', 'archive'],
  ])('refuses organize with %s', (_name, patch) => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at))
    expect(() => store.organize('chat_a', patch)).toThrow()
    expect(store.get('chat_a')).toMatchObject({ group_id: null, archived_at: null })
  })

  it('renames, folds and deletes groups; deleting leaves its conversations alone', () => {
    const store = createChatHistoryStore(dir)
    expect(store.groups()).toEqual({ groups: [], assistant_collapsed: false, archive_collapsed: true })
    store.createGroup('甲')
    const { groups: [a, b] } = store.createGroup('乙')
    expect([a.name, b.name]).toEqual(['甲', '乙'])
    expect(store.renameGroup(b.id, '丙').groups.map(g => g.name)).toEqual(['甲', '丙'])
    expect(store.setCollapsed(a.id, true).groups[0].collapsed).toBe(true)
    expect(store.setCollapsed('assistant', true).assistant_collapsed).toBe(true)
    expect(store.setCollapsed('archive', false).archive_collapsed).toBe(false)
    store.save(record('chat_a', at))
    store.organize('chat_a', { group_id: a.id })
    expect(store.deleteGroup(a.id).groups.map(g => g.name)).toEqual(['丙'])
    // The conversation is not rewritten: its group just no longer exists.
    expect(store.get('chat_a')?.group_id).toBe(a.id)
    // Unknown groups change nothing.
    expect(store.renameGroup('grp_none', 'x').groups).toHaveLength(1)
    expect(store.deleteGroup('grp_none').groups).toHaveLength(1)
    // Survives a new store; clearing the history keeps the groups.
    store.clear()
    expect(createChatHistoryStore(dir).groups()).toMatchObject({ groups: [{ name: '丙' }], assistant_collapsed: true, archive_collapsed: false })
    expect(store.list()).toEqual([])
  })

  it.each([
    ['an empty name', () => createChatHistoryStore(dir).createGroup(' \n ')],
    ['a name that is not text', () => createChatHistoryStore(dir).createGroup(3)],
    ['an overlong name', () => createChatHistoryStore(dir).createGroup('x'.repeat(1001))],
    ['a bad group id', () => createChatHistoryStore(dir).renameGroup('../x', 'a')],
    ['a bad fold target', () => createChatHistoryStore(dir).setCollapsed('../x', true)],
    ['a non-boolean fold', () => createChatHistoryStore(dir).setCollapsed('assistant', 'yes')],
  ])('refuses a group change with %s', (_name, change) => {
    expect(change).toThrow()
  })

  it('stops at 200 groups', () => {
    const store = createChatHistoryStore(dir)
    for (let i = 0; i < 200; i++) store.createGroup(`组${i}`)
    expect(() => store.createGroup('多一个')).toThrow('Too many chat groups')
  })

  it('reads a damaged or foreign groups file as no groups; a change sets it aside, not over it', () => {
    fs.mkdirSync(path.join(dir, 'chat-history'))
    const file = path.join(dir, 'chat-history', '_groups.json')
    for (const body of ['{', JSON.stringify({ format: 'x', version: 1, groups: [], assistant_collapsed: false, archive_collapsed: true }),
      JSON.stringify({ format: 'agent24.desktop.chat-groups', version: 1, groups: [{ id: 'grp_a', name: '', created_at: at, collapsed: false }], assistant_collapsed: false, archive_collapsed: true }),
      JSON.stringify({ format: 'agent24.desktop.chat-groups', version: 1, groups: [{ id: 'grp_a', name: '甲', created_at: at, collapsed: false }, { id: 'grp_a', name: '乙', created_at: at, collapsed: false }], assistant_collapsed: false, archive_collapsed: true })]) {
      fs.writeFileSync(file, body)
      expect(createChatHistoryStore(dir).groups().groups).toEqual([])
    }
    const damaged = fs.readFileSync(file, 'utf8')
    const open = vi.spyOn(fs, 'openSync')
    createChatHistoryStore(dir).createGroup('新')
    expect(open).toHaveBeenCalledWith(expect.stringMatching(/_groups\.json\.[0-9a-f]{12}\.tmp$/), 'wx', 0o600)
    expect(JSON.parse(fs.readFileSync(file, 'utf8'))).toMatchObject({ format: 'agent24.desktop.chat-groups', version: 1 })
    // Damaged again: that one is set aside as well, beside the first.
    fs.writeFileSync(file, '[')
    createChatHistoryStore(dir).createGroup('又')
    const root = path.join(dir, 'chat-history')
    const aside = fs.readdirSync(root).filter(n => /^_groups\.damaged\.\d+\.[0-9a-f]{8}\.json$/.test(n))
    expect(aside.map(n => fs.readFileSync(path.join(root, n), 'utf8')).sort()).toEqual([damaged, '['].sort())
    expect(fs.readdirSync(root)).toHaveLength(3)
  })

  it('a fold with a bad target is refused before the groups file is read for a change (PR-Daemon #1647 L1)', () => {
    fs.mkdirSync(path.join(dir, 'chat-history'))
    const file = path.join(dir, 'chat-history', '_groups.json')
    fs.writeFileSync(file, '{')
    expect(() => createChatHistoryStore(dir).setCollapsed('../x', true)).toThrow('Invalid chat group id')
    // Left as it was: not set aside for a change that never happened.
    expect(fs.readdirSync(path.join(dir, 'chat-history'))).toEqual(['_groups.json'])
    expect(fs.readFileSync(file, 'utf8')).toBe('{')
  })

  it('refuses a group change when the groups file is there but cannot be read', () => {
    const store = createChatHistoryStore(dir)
    store.createGroup('甲')
    const file = path.join(dir, 'chat-history', '_groups.json')
    const before = fs.readFileSync(file, 'utf8')
    const real = fs.readFileSync
    vi.spyOn(fs, 'readFileSync').mockImplementation(((f: fs.PathOrFileDescriptor, o?: unknown) => {
      if (f === file) throw Object.assign(new Error('EACCES'), { code: 'EACCES' })
      return real(f, o as never)
    }) as typeof fs.readFileSync)
    expect(store.groups().groups).toEqual([])
    for (const change of [() => store.createGroup('乙'), () => store.setCollapsed('assistant', true)]) {
      expect(change).toThrow('Chat groups unreadable')
    }
    vi.restoreAllMocks()
    expect(fs.readFileSync(file, 'utf8')).toBe(before)
  })

  it('deleting a group and clearing remove temporary files of interrupted group writes', () => {
    const store = createChatHistoryStore(dir)
    const { groups: [a] } = store.createGroup('甲')
    const root = path.join(dir, 'chat-history')
    const stray = () => fs.writeFileSync(path.join(root, '_groups.json.0123456789ab.tmp'), '{"groups":[{"name":"甲"}]}')
    stray()
    fs.writeFileSync(path.join(root, '_groups.json.keep'), 'x')
    store.deleteGroup(a.id)
    expect(fs.readdirSync(root).sort()).toEqual(['_groups.json', '_groups.json.keep'])
    // A deletion asked again after one that stopped before its sweep.
    stray()
    store.deleteGroup(a.id)
    expect(fs.readdirSync(root).sort()).toEqual(['_groups.json', '_groups.json.keep'])
    stray()
    store.clear()
    expect(fs.readdirSync(root).sort()).toEqual(['_groups.json', '_groups.json.keep'])
  })
})

describe('runs and waiting items (M4 A2)', () => {
  const at = '2026-11-08T01:00:00.000Z'
  const onDisk = (id: string) => JSON.parse(fs.readFileSync(path.join(dir, 'chat-history', `${id}.json`), 'utf8'))
  const reply = (runId: string | null, when: string) =>
    ({ role: 'assistant' as const, content: 'a', at: when, run_id: runId, latency: null })
  const withReplies = (...runIds: (string | null)[]): ChatHistoryRecord => ({
    ...record('chat_a', at),
    messages: [{ role: 'user', content: '你好', at, delivered: true }, ...runIds.map((r, i) => reply(r, `2026-11-08T01:00:0${i + 1}.000Z`))],
  })
  const ask = (runId?: string) => ({ id: 'ask_1', kind: 'confirm', label: '加进对话？', at, ...(runId ? { run_id: runId } : {}) })
  afterEach(() => { onChatHistoryChange(null) })

  it('a save records each reply\'s run as an answer, once, at the reply\'s time; answers are no tasks', () => {
    const store = createChatHistoryStore(dir)
    store.save(withReplies('run_1', null))
    const summary = store.save(withReplies('run_1', null, 'run_2'))
    expect(onDisk('chat_a').runs).toEqual([
      { run_id: 'run_1', kind: 'answer', started_at: '2026-11-08T01:00:01.000Z' },
      { run_id: 'run_2', kind: 'answer', started_at: '2026-11-08T01:00:03.000Z' },
    ])
    expect(summary.tasks).toEqual([])
  })

  it('a save never sets runs or waiting items: what the page sends is ignored, what is on file kept', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at))
    store.promoteRun('chat_a', 'run_9')
    store.setPending('chat_a', ask('run_9'))
    store.save({
      ...record('chat_a', '2026-11-08T02:00:00.000Z'),
      runs: [{ run_id: 'run_x', kind: 'task', started_at: at }],
      pending: [{ id: 'fake', kind: 'approval', label: 'x', at }],
    })
    expect(onDisk('chat_a').runs).toEqual([expect.objectContaining({ run_id: 'run_9', kind: 'task' })])
    expect(onDisk('chat_a').pending).toEqual([ask('run_9')])
  })

  it('promoteRun makes an answer a task, records an unknown run as a task, and leaves a schedule\'s run alone', () => {
    const store = createChatHistoryStore(dir)
    store.save(withReplies('run_1'))
    expect(store.promoteRun('chat_a', 'run_1')?.tasks).toEqual([{ run_id: 'run_1', kind: 'task', started_at: '2026-11-08T01:00:01.000Z' }])
    expect(store.promoteRun('chat_a', 'run_2')?.tasks.map(r => r.run_id)).toEqual(['run_1', 'run_2'])
    store.appendRun('chat_a', 'run_3', 'schedule')
    store.promoteRun('chat_a', 'run_3')
    expect(onDisk('chat_a').runs.map((r: { kind: string }) => r.kind)).toEqual(['task', 'task', 'schedule'])
    // A task is not made an answer again.
    store.appendRun('chat_a', 'run_1', 'answer')
    expect(onDisk('chat_a').runs[0].kind).toBe('task')
  })

  it('changes only a conversation on file, never its messages or updated_at', () => {
    const store = createChatHistoryStore(dir)
    expect(store.promoteRun('chat_none', 'run_1')).toBeNull()
    expect(store.setPending('chat_none', ask())).toBeNull()
    expect(fs.existsSync(path.join(dir, 'chat-history', 'chat_none.json'))).toBe(false)
    store.save(record('chat_a', at))
    const before = onDisk('chat_a')
    store.promoteRun('chat_a', 'run_1')
    store.setPending('chat_a', ask())
    expect(onDisk('chat_a')).toMatchObject({ messages: before.messages, updated_at: before.updated_at })
  })

  it('setPending adds or replaces by id; clearPending takes it off, and nothing when it is not there', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at))
    store.setPending('chat_a', ask())
    expect(store.setPending('chat_a', { ...ask('run_1'), label: '改过' })?.pending).toEqual([{ ...ask('run_1'), label: '改过' }])
    expect(store.clearPending('chat_a', 'other')?.pending).toHaveLength(1)
    expect(store.clearPending('chat_a', 'ask_1')?.pending).toEqual([])
  })

  it.each([
    ['a bad id', { ...{ id: '../x', kind: 'confirm', label: 'x', at } }],
    ['an unknown kind', { id: 'a', kind: 'maybe', label: 'x', at }],
    ['an empty label', { id: 'a', kind: 'confirm', label: '  ', at }],
    ['a bad time', { id: 'a', kind: 'confirm', label: 'x', at: 'now' }],
    ['a bad run', { id: 'a', kind: 'approval', label: 'x', at, run_id: 'a/b' }],
  ])('refuses a waiting item with %s', (_name, item) => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at))
    expect(() => store.setPending('chat_a', item)).toThrow('Invalid chat history pending item')
  })

  it('refuses a bad run id or kind, and a run past the cap', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at))
    expect(() => store.promoteRun('chat_a', '../x')).toThrow('Invalid chat history run')
    expect(() => store.appendRun('chat_a', 'run_1', 'job')).toThrow('Invalid chat history run kind')
    const full = { ...onDisk('chat_a'), runs: Array.from({ length: 1000 }, (_, i) => ({ run_id: `run_${i}`, kind: 'task', started_at: at })) }
    fs.writeFileSync(path.join(dir, 'chat-history', 'chat_a.json'), JSON.stringify(full))
    expect(() => store.promoteRun('chat_a', 'run_new')).toThrow('Too many chat history runs')
  })

  it('makes room for a task past the cap by dropping the oldest answer; refuses an answer (PR-Daemon #1696 L1)', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at))
    const runs = Array.from({ length: 1000 }, (_, i) => ({ run_id: `run_${i}`, kind: i === 0 ? 'task' : 'answer', started_at: at }))
    fs.writeFileSync(path.join(dir, 'chat-history', 'chat_a.json'), JSON.stringify({ ...onDisk('chat_a'), runs }))
    expect(() => store.appendRun('chat_a', 'run_new_answer', 'answer')).toThrow('Too many chat history runs')
    store.promoteRun('chat_a', 'run_new')
    const after = onDisk('chat_a').runs.map((r: { run_id: string }) => r.run_id)
    expect(after).toHaveLength(1000)
    expect(after[0]).toBe('run_0')
    expect(after).not.toContain('run_1')
    expect(after[999]).toBe('run_new')
  })

  it('refuses a waiting item past the cap, and a bad id to clear', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at))
    const full = { ...onDisk('chat_a'), pending: Array.from({ length: 100 }, (_, i) => ({ id: `p${i}`, kind: 'confirm', label: 'x', at })) }
    fs.writeFileSync(path.join(dir, 'chat-history', 'chat_a.json'), JSON.stringify(full))
    expect(() => store.setPending('chat_a', ask())).toThrow('Too many chat history pending items')
    // Replacing one already there is no new item.
    expect(store.setPending('chat_a', { id: 'p0', kind: 'confirm', label: 'y', at })?.pending).toHaveLength(100)
    expect(() => store.clearPending('chat_a', '../x')).toThrow('Invalid chat history pending item')
  })

  it.each([
    ['a duplicate run', { runs: [{ run_id: 'run_1', kind: 'task', started_at: at }, { run_id: 'run_1', kind: 'answer', started_at: at }] }],
    ['an unknown run kind', { runs: [{ run_id: 'run_1', kind: 'job', started_at: at }] }],
    ['runs past the cap', { runs: Array.from({ length: 1001 }, (_, i) => ({ run_id: `r${i}`, kind: 'task', started_at: at })) }],
    ['waiting items past the cap', { pending: Array.from({ length: 101 }, (_, i) => ({ id: `p${i}`, kind: 'confirm', label: 'x', at })) }],
    ['a duplicate waiting item', { pending: [{ id: 'a', kind: 'confirm', label: 'x', at }, { id: 'a', kind: 'confirm', label: 'y', at }] }],
  ])('refuses a file with %s', (_name, patch) => {
    expect(() => parseChatHistoryRecord({ ...record('chat_a', at), ...patch })).toThrow('Invalid chat history record')
  })

  it('tells the change listener each written change, with the summary as it now stands, and nothing else', () => {
    const store = createChatHistoryStore(dir)
    const told: unknown[] = []
    onChatHistoryChange(summary => { told.push(summary) })
    store.save(record('chat_a', at))
    store.promoteRun('chat_a', 'run_1')
    store.promoteRun('chat_a', 'run_1')
    store.clearPending('chat_a', 'none')
    store.promoteRun('chat_none', 'run_1')
    expect(told).toEqual([expect.objectContaining({ id: 'chat_a', tasks: [expect.objectContaining({ run_id: 'run_1' })] })])
  })

  it('refuses a save or a change, rather than lose its runs, when the file is there but cannot be read (Codex A2 C1)', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at))
    store.promoteRun('chat_a', 'run_1')
    const file = path.join(dir, 'chat-history', 'chat_a.json')
    fs.chmodSync(file, 0o000)
    try {
      expect(() => store.save(record('chat_a', '2026-11-08T02:00:00.000Z'))).toThrow('Chat history unreadable')
      expect(() => store.setPending('chat_a', ask())).toThrow('Chat history unreadable')
      // Only showing it reads it as missing.
      expect(store.get('chat_a')).toBeNull()
    } finally { fs.chmodSync(file, 0o600) }
    expect(onDisk('chat_a').runs).toEqual([expect.objectContaining({ run_id: 'run_1', kind: 'task' })])
  })

  it('does not count the page\'s runs toward the size a save may write: it never writes them (Codex A2 C3)', () => {
    const store = createChatHistoryStore(dir)
    const big = { ...record('chat_a', at), messages: [{ role: 'user' as const, content: 'x'.repeat(8 * 1024 * 1024 - 40_000), at, delivered: true }] }
    const runs = Array.from({ length: 1000 }, (_, i) => ({ run_id: `r${i}`, kind: 'task' as const, started_at: at }))
    expect(() => store.save({ ...big, runs })).not.toThrow()
    expect(onDisk('chat_a').runs).toEqual([])
  })

  it('writes the file 0600 by atomic replacement, leaving no temporary file', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', at))
    store.promoteRun('chat_a', 'run_1')
    store.setPending('chat_a', ask())
    expect(fs.statSync(path.join(dir, 'chat-history', 'chat_a.json')).mode & 0o777).toBe(0o600)
    expect(fs.readdirSync(path.join(dir, 'chat-history'))).toEqual(['chat_a.json'])
  })
})

// M4 A5: conversations main starts itself, named after a schedule.
describe('conversations main starts (M4 A5)', () => {
  it('starts an empty conversation with its title, and tells the window', () => {
    const store = createChatHistoryStore(dir)
    const told = vi.fn()
    onChatHistoryChange(told)
    try {
      const summary = store.create({ title: '日程：周报', title_source: 'schedule' })
      expect(summary).toMatchObject({ title: '日程：周报', title_source: 'schedule', group_id: null, archived_at: null, tasks: [], pending: [] })
      expect(summary.id).toMatch(/^chat_[a-z0-9_]+$/)
      expect(store.get(summary.id)).toMatchObject({ messages: [], runs: [] })
      expect(told).toHaveBeenCalledWith(summary)
      expect(store.create({ title: 'x', title_source: 'schedule' }).id).not.toBe(summary.id)
    } finally {
      onChatHistoryChange(null)
    }
  })

  it.each([
    [{ title: 3, title_source: 'schedule' }],
    [{ title: 'x', title_source: 'other' }],
    [{ title: 'x'.repeat(4001), title_source: 'schedule' }],
  ])('refuses %j', input => {
    const store = createChatHistoryStore(dir)
    expect(() => store.create(input as never)).toThrow('Invalid chat history conversation')
    expect(store.list()).toEqual([])
  })

  it('keeps the schedule\'s name over the first prompt and the model\'s title, not over the user\'s rename', () => {
    const store = createChatHistoryStore(dir)
    const { id } = store.create({ title: '日程：周报', title_source: 'schedule' })
    // The user asks in it: the page saves with the first prompt's title.
    const page = { ...record(id, '2026-11-09T00:00:00.000Z', '这周的呢'), id }
    expect(store.save(page)).toMatchObject({ title: '日程：周报', title_source: 'schedule' })
    expect(store.setTitle(id, '模型标题', 'model')).toMatchObject({ title: '日程：周报', title_source: 'schedule' })
    expect(store.setTitle(id, '我的周报', 'user')).toMatchObject({ title: '我的周报', title_source: 'user' })
  })

  it('keeps the assistant\'s conversation\'s name (A6) the same way', () => {
    const store = createChatHistoryStore(dir)
    const { id } = store.create({ title: '助理发起', title_source: 'assistant' })
    expect(store.save({ ...record(id, '2026-11-09T00:00:00.000Z', '问一句'), id })).toMatchObject({ title: '助理发起', title_source: 'assistant' })
    expect(store.setTitle(id, '模型标题', 'model')).toMatchObject({ title: '助理发起', title_source: 'assistant' })
    expect(store.setTitle(id, '我的', 'user')).toMatchObject({ title: '我的', title_source: 'user' })
  })
})
