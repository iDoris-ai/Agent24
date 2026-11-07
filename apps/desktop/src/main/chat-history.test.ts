import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createChatHistoryStore, parseChatHistoryRecord } from './chat-history'
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
    title_source: 'prompt',
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
    expect(raw.version).toBe(1)
    expect(raw.messages.every((m: { at?: string }) => typeof m.at === 'string')).toBe(true)
  })

  it('overwrites a conversation in place, deletes one, and clears only its own files', () => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', '2026-11-08T01:00:00.000Z'))
    store.save(record('chat_a', '2026-11-08T03:00:00.000Z', 'renamed'))
    store.save(record('chat_b', '2026-11-08T02:00:00.000Z'))
    expect(store.list()).toEqual([
      { id: 'chat_a', title: 'renamed', title_source: 'prompt', created_at: '2026-11-08T00:00:00.000Z', updated_at: '2026-11-08T03:00:00.000Z', first_prompt: '你好' },
      { id: 'chat_b', title: 'chat_b', title_source: 'prompt', created_at: '2026-11-08T00:00:00.000Z', updated_at: '2026-11-08T02:00:00.000Z', first_prompt: '你好' },
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
    ['another version', { version: 2 }],
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

  it('reads a version 1 file written before title_source as the first prompt\'s title', () => {
    const old: Partial<ChatHistoryRecord> = record('chat_a', '2026-11-08T01:00:00.000Z', '旧标题')
    delete old.title_source
    fs.mkdirSync(path.join(dir, 'chat-history'))
    fs.writeFileSync(path.join(dir, 'chat-history', 'chat_a.json'), JSON.stringify(old))
    const store = createChatHistoryStore(dir)
    expect(store.get('chat_a')).toMatchObject({ title: '旧标题', title_source: 'prompt' })
    expect(store.list()).toEqual([expect.objectContaining({ id: 'chat_a', title_source: 'prompt' })])
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
      { id: 'chat_a', title: '问', title_source: 'prompt', created_at: '2026-11-08T00:00:00.000Z', updated_at: at, first_prompt: '你好' })
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
