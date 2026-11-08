import { describe, expect, it, vi } from 'vitest'
import type { ChatHistoryMessage, ChatHistoryRecord } from '../shared/chat-history'
import type { BackendProxyResponse } from '../shared/ipc-types'
import { MAX_ENTRY_TITLE } from './knowledge-entry-draft'
import {
  MAX_SOURCE_CHARS,
  SourceTooLong,
  clearFindings,
  entryTarget,
  extractEntry,
  fallbackTitle,
  findings,
  sourceMessages,
  type ChatCall,
} from './knowledge-entry-extract'
import { placeholder } from './knowledge-entry-redact'

const AT = '2026-11-09T12:00:00.000Z'
const user = (content: string): ChatHistoryMessage => ({ role: 'user', content, at: AT, delivered: true })
const reply = (content: string): ChatHistoryMessage => ({ role: 'assistant', content, at: AT, run_id: null, latency: null } as ChatHistoryMessage)
const record = (messages: ChatHistoryMessage[], title = '对话'): ChatHistoryRecord =>
  ({ format: 'agent24-chat-history', version: 1, id: 'chat_abc', title, title_source: 'prompt', group_id: null, archived_at: null, created_at: AT, updated_at: AT, messages } as unknown as ChatHistoryRecord)

const ok = (content: unknown): BackendProxyResponse => ({ ok: true, status: 200, data: { message: { role: 'assistant', content } } })
const fail = (status: number, code: string): BackendProxyResponse => ({ ok: false, status, data: { error: { code, message: 'x' } } })

describe('sourceMessages: the pre-redaction', () => {
  it('redacts every chosen message with the rule table and keeps only role and text', () => {
    const r = record([
      user('我的邮箱 alice@example.com，密码 password=hunter2'),
      reply('日志在 /Users/alice/logs/app.log，服务在 10.0.0.5:8080'),
      user('谢谢'),
    ])
    const source = sourceMessages(r, 0, 1)
    expect(source).toEqual([
      { role: 'user', text: `我的邮箱 ${placeholder('email')}，密码 password=${placeholder('credential')}` },
      { role: 'assistant', text: `日志在 ${placeholder('path')}，服务在 ${placeholder('internal_address')}` },
    ])
    const sent = JSON.stringify(source)
    for (const secret of ['alice@example.com', 'hunter2', '/Users/alice', '10.0.0.5']) expect(sent).not.toContain(secret)
  })

  it('refuses a range longer than MAX_SOURCE_CHARS instead of cutting it', () => {
    const half = 'a'.repeat(MAX_SOURCE_CHARS / 2 + 1)
    expect(() => sourceMessages(record([user(half), reply(half)]), 0, 1)).toThrow(SourceTooLong)
    expect(sourceMessages(record([user(half), reply(half)]), 0, 0)).toHaveLength(1)
  })
})

describe('fallbackTitle', () => {
  it('is the conversation title redacted on one line, or the untitled name', () => {
    expect(fallbackTitle(' 发给\nbob@example.com 的 ', '未命名')).toBe(`发给 ${placeholder('email')} 的`)
    expect(fallbackTitle('   ', '未命名')).toBe('未命名')
    expect(fallbackTitle('长'.repeat(500), '未命名').length).toBe(MAX_ENTRY_TITLE)
    expect(fallbackTitle('a\ud800', '未命名')).toBe('a�')
  })
})

describe('extractEntry: the model step', () => {
  const source = [{ role: 'user' as const, text: '报销什么时候交？' }, { role: 'assistant' as const, text: '每月 25 日前。' }]

  it('asks once, with no session_id and no tools, and takes the model draft', async () => {
    const call = vi.fn<ChatCall>(async () => ok(JSON.stringify({ kind: 'faq', title: '报销截止', body: '每月 25 日前交。', hints: ['25 日'] })))
    const ex = await extractEntry(source, '未命名', call)
    expect(ex).toEqual({ kind: 'faq', title: '报销截止', body: '每月 25 日前交。', hints: ['25 日'], model: 'drafted' })
    expect(call).toHaveBeenCalledTimes(1)
    const body = call.mock.calls[0][0] as Record<string, unknown>
    expect(Object.keys(body)).toEqual(['messages'])
    expect(JSON.stringify(body)).not.toContain('session_id')
  })

  it('gives the empty template and says why when there is no usable model', async () => {
    const cases: Array<[ChatCall, string]> = [
      [async () => fail(503, 'provider_unavailable'), 'no_model'],
      [async () => fail(409, 'idoris_headroom_confirmation_required'), 'needs_confirmation'],
      [async () => fail(503, 'idoris_supervisor_busy'), 'busy'],
      [async () => fail(500, 'internal'), 'failed'],
      [async () => fail(504, 'timeout'), 'failed'],
      [async () => ({ ok: false, status: 502, data: null }), 'failed'],
      [async () => { throw new Error('socket') }, 'failed'],
      [async () => ok('我不会写 JSON'), 'unusable'],
      [async () => ok(null), 'unusable'],
      [async () => ({ ok: true, status: 200, data: null }), 'unusable'],
    ]
    for (const [call, model] of cases) {
      expect(await extractEntry(source, '未命名知识条目', call)).toEqual({ kind: 'note', title: '未命名知识条目', body: '', hints: [], model })
    }
  })

  it('puts its own title in when the model gave none', async () => {
    const ex = await extractEntry(source, '对话标题', async () => ok(JSON.stringify({ kind: 'note', title: '  ', body: 'b' })))
    expect(ex.title).toBe('对话标题')
    expect(ex.model).toBe('drafted')
  })
})

describe('findings: the post-scan', () => {
  it('finds what a model let slip, in rule-table order, in the title and the body (QA §3: a stub that misses one)', () => {
    expect(findings('联系 bob@example.com', '密钥 sk-abcdefghijklmnopqrstuvwx，路径 /Users/bob/x')).toEqual(['credential', 'email', 'path'])
  })

  it('finds nothing in text the rule table redacted', () => {
    expect(findings(placeholder('email'), `见 ${placeholder('path')}`)).toEqual([])
  })
})

describe('entryTarget: what a draft is bound to (QA N11)', () => {
  it('names the endpoint and the set of knowledge bases, in any order', () => {
    const a = entryTarget('https://kb.example', ['kb2', 'kb1'])
    expect(a).toMatch(/^kbt1:[0-9a-f]{64}$/)
    expect(entryTarget('https://kb.example', ['kb1', 'kb2', 'kb1'])).toBe(a)
    expect(entryTarget('https://kb.example', ['kb1'])).not.toBe(a)
    expect(entryTarget('https://other.example', ['kb1', 'kb2'])).not.toBe(a)
    expect(entryTarget('https://kb.example', [])).not.toBe(entryTarget('https://kb.example', ['']))
  })
})

describe('clearFindings: "一键脱敏"', () => {
  it('clears what the post-scan holds back beyond the rule table (PR-Daemon #1677 non-blocking 1)', () => {
    const texts = [
      '链接 file:///Users/alice/notes.txt 里有',
      '订单 123456789012345 已发',
      '见 https://example.com/home/alice/ 页面',
      `password="${' '.repeat(70)}hunter2`,
    ]
    for (const text of texts) {
      expect(findings('t', text), text).not.toEqual([])
      const out = clearFindings(text)
      expect(findings('t', out), out).toEqual([])
    }
    expect(clearFindings('链接 file:///Users/alice/notes.txt 里有')).toBe(`链接 ${placeholder('path')} 里有`)
  })

  it('leaves clean text as it is', () => {
    expect(clearFindings('每月 25 日前交到财务。')).toBe('每月 25 日前交到财务。')
  })
})

