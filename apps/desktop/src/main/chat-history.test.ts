import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { createChatHistoryStore, parseChatHistoryRecord } from './chat-history'
import { CHAT_HISTORY_FORMAT, CHAT_HISTORY_VERSION, type ChatHistoryRecord } from '../shared/chat-history'

let dir: string
beforeEach(() => { dir = fs.mkdtempSync(path.join(os.tmpdir(), 'chat-history-')) })
afterEach(() => { fs.rmSync(dir, { recursive: true, force: true }) })

function record(id: string, updated: string, title = id): ChatHistoryRecord {
  return {
    format: CHAT_HISTORY_FORMAT,
    version: CHAT_HISTORY_VERSION,
    id,
    title,
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
      { id: 'chat_a', title: 'renamed', created_at: '2026-11-08T00:00:00.000Z', updated_at: '2026-11-08T03:00:00.000Z' },
      { id: 'chat_b', title: 'chat_b', created_at: '2026-11-08T00:00:00.000Z', updated_at: '2026-11-08T02:00:00.000Z' },
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
    ['a user message without delivery', { messages: [{ role: 'user', content: 'x', at: '2026-11-08T00:00:00.000Z' }] }],
  ])('refuses a record with %s and keeps the previous file', (_name, patch) => {
    const store = createChatHistoryStore(dir)
    store.save(record('chat_a', '2026-11-08T01:00:00.000Z'))
    expect(() => store.save({ ...record('chat_a', '2026-11-08T05:00:00.000Z'), ...patch })).toThrow()
    expect(store.get('chat_a')?.updated_at).toBe('2026-11-08T01:00:00.000Z')
  })

  it('drops unknown fields instead of storing them', () => {
    const parsed = parseChatHistoryRecord({ ...record('chat_a', '2026-11-08T01:00:00.000Z'), extra: 'x' })
    expect(parsed).not.toHaveProperty('extra')
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
