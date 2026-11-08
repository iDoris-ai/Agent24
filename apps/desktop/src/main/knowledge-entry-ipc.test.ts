import type { IpcMainInvokeEvent } from 'electron'
import { describe, expect, it, vi } from 'vitest'

import type { ChatHistoryMessage, ChatHistoryRecord } from '../shared/chat-history'
import { IpcChannels } from '../shared/ipc-types'
import type { AdmissionReader, FloorReading } from './knowledge-entry-admission'
import { knowledgeEntryAdmitter, registerKnowledgeEntryIpc, shortTitle, type KnowledgeEntryDeps } from './knowledge-entry-ipc'

const AT = '2026-11-09T12:00:00.000Z'
const event = {} as IpcMainInvokeEvent
const user = (content: string): ChatHistoryMessage => ({ role: 'user', content, at: AT, delivered: true })
const reply = (content: string, run_id: string | null = null): ChatHistoryMessage => ({ role: 'assistant', content, at: AT, run_id, latency: null })

function conversation(messages: ChatHistoryMessage[], title = '请假流程'): ChatHistoryRecord {
  return { version: 1, id: 'chat_abc', title, title_source: 'prompt', created_at: AT, updated_at: AT, group_id: null, archived_at: null, messages } as unknown as ChatHistoryRecord
}

function setup(over: Partial<KnowledgeEntryDeps> & { reading?: FloorReading; answer?: boolean } = {}) {
  let record: ChatHistoryRecord | null = conversation([user('怎么请假'), reply('打开人事系统')])
  const reads: string[] = []
  const reader: AdmissionReader = { read: async (id) => { reads.push(id); return over.reading ?? 'ordinary' } }
  type Confirm = KnowledgeEntryDeps['confirm']
  const confirm = vi.fn<Confirm>(over.confirm ?? (async () => over.answer ?? true))
  const deps: KnowledgeEntryDeps = {
    conversation: (id) => (id === 'chat_abc' ? record : null),
    reader,
    locale: () => 'zh',
    bound: () => true,
    ...over,
    confirm,
  }
  const admitter = knowledgeEntryAdmitter(deps)
  return { admit: admitter.admit, judge: admitter.judge, confirm, reads, set: (next: ChatHistoryRecord | null) => { record = next } }
}

describe('knowledgeEntryAdmitter', () => {
  it('asks the user in main’s own words and admits on yes (N10a)', async () => {
    const s = setup()
    expect(await s.admit(event, 'chat_abc')).toEqual({ ok: true })
    expect(s.confirm).toHaveBeenCalledTimes(1)
    const ask = s.confirm.mock.calls[0][1] as { message: string; detail: string }
    expect(ask.message).toBe('把对话「请假流程」沉淀为知识条目？')
    expect(ask.detail).toContain('工具结果、记忆和附件不会被使用')
    // Judged before asking and again after.
    expect(s.reads).toEqual(['chat_abc', 'chat_abc'])
  })

  it('a no is not an admission (N10b)', async () => {
    const s = setup({ answer: false })
    expect(await s.admit(event, 'chat_abc')).toEqual({ ok: false, reason: 'not_confirmed' })
  })

  it('a dialog that fails counts as no', async () => {
    const s = setup({ confirm: vi.fn(async () => { throw new Error('no window') }) })
    expect(await s.admit(event, 'chat_abc')).toEqual({ ok: false, reason: 'not_confirmed' })
  })

  it('refuses Restricted, Unknown and a missing floor without asking anything (N10c–e)', async () => {
    for (const [reading, reason] of [['restricted', 'restricted'], ['unknown', 'unknown'], ['missing', 'floor_missing']] as const) {
      const s = setup({ reading })
      expect(await s.admit(event, 'chat_abc')).toEqual({ ok: false, reason })
      expect(s.confirm).not.toHaveBeenCalled()
    }
  })

  it('judges the conversation again as it is once the user answered', async () => {
    let calls = 0
    const reader: AdmissionReader = { read: async () => (++calls === 1 ? 'ordinary' : 'restricted') }
    const s = setup({ reader })
    expect(await s.admit(event, 'chat_abc')).toEqual({ ok: false, reason: 'restricted' })
    const gone = setup()
    gone.confirm.mockImplementation(async () => { gone.set(null); return true })
    expect(await gone.admit(event, 'chat_abc')).toEqual({ ok: false, reason: 'not_found' })
  })

  it('reads only main’s own copy: an unknown, malformed or throwing id is not found', async () => {
    const s = setup()
    expect(await s.admit(event, 'chat_other')).toEqual({ ok: false, reason: 'not_found' })
    expect(await s.admit(event, { id: 'chat_abc', messages: [] })).toEqual({ ok: false, reason: 'not_found' })
    const throwing = setup({ conversation: () => { throw new Error('bad id') } })
    expect(await throwing.admit(event, '../x')).toEqual({ ok: false, reason: 'not_found' })
    expect(s.confirm).not.toHaveBeenCalled()
  })

  it('opens one dialog at a time and turns a second request away', async () => {
    let release: (yes: boolean) => void = () => {}
    const s = setup({ confirm: vi.fn(() => new Promise<boolean>((resolve) => { release = resolve })) })
    const first = s.admit(event, 'chat_abc')
    await vi.waitFor(() => expect(s.confirm).toHaveBeenCalledTimes(1))
    expect(await s.admit(event, 'chat_abc')).toEqual({ ok: false, reason: 'busy' })
    release(true)
    expect(await first).toEqual({ ok: true })
    // Free again afterwards.
    const next = s.admit(event, 'chat_abc')
    await vi.waitFor(() => expect(s.confirm).toHaveBeenCalledTimes(2))
    release(false)
    expect(await next).toEqual({ ok: false, reason: 'not_confirmed' })
  })
})

describe('judge', () => {
  it('hands main the very record it judged, and nothing on a refusal', async () => {
    const s = setup()
    const later = conversation([user('怎么请假'), reply('打开人事系统'), user('还有呢'), reply('找主管')])
    s.confirm.mockImplementation(async () => { s.set(later); return true })
    const judged = await s.judge(event, 'chat_abc')
    expect(judged.admission).toEqual({ ok: true })
    expect(judged.record).toBe(later)
    const refused = setup({ reading: 'restricted' })
    expect(await refused.judge(event, 'chat_abc')).toEqual({ admission: { ok: false, reason: 'restricted' }, record: null })
    const no = setup({ answer: false })
    expect((await no.judge(event, 'chat_abc')).record).toBeNull()
  })

  it('judges the record it hands back: a Restricted run added during the dialog refuses', async () => {
    // A reader that answers from the runs it is asked about.
    const reader: AdmissionReader = { read: async (_id, runs) => (runs.includes('run_secret') ? 'restricted' : 'ordinary') }
    const s = setup({ reader })
    s.confirm.mockImplementation(async () => {
      s.set(conversation([user('怎么请假'), reply('打开人事系统'), user('机密呢'), reply('…', 'run_secret')]))
      return true
    })
    expect(await s.judge(event, 'chat_abc')).toEqual({ admission: { ok: false, reason: 'restricted' }, record: null })
  })
})

describe('knowledgeEntryAdmitter while Knowledge is off', () => {
  it('refuses before reading anything, and again if it went off during the dialog', async () => {
    const off = setup({ bound: () => false })
    expect(await off.admit(event, 'chat_abc')).toEqual({ ok: false, reason: 'knowledge_off' })
    expect(off.reads).toEqual([])
    expect(off.confirm).not.toHaveBeenCalled()
    let on = true
    const flips = setup({ bound: () => on })
    flips.confirm.mockImplementation(async () => { on = false; return true })
    expect(await flips.admit(event, 'chat_abc')).toEqual({ ok: false, reason: 'knowledge_off' })
  })
})

describe('shortTitle', () => {
  it('keeps short titles, cuts long ones by character, and names an empty one', () => {
    expect(shortTitle('  请假\n流程 ', '未命名对话')).toBe('请假 流程')
    expect(shortTitle('', '未命名对话')).toBe('未命名对话')
    const long = '🌿'.repeat(61)
    expect(shortTitle(long, 'x')).toBe(`${'🌿'.repeat(60)}…`)
  })
})

describe('registerKnowledgeEntryIpc', () => {
  it('handles one channel through the guard', () => {
    const handle = vi.fn()
    const guard = vi.fn(<F,>(f: F) => f)
    const deps: KnowledgeEntryDeps = { conversation: () => null, reader: { read: async () => 'ordinary' }, confirm: async () => true, locale: () => 'en', bound: () => true }
    registerKnowledgeEntryIpc({ handle } as never, guard as never, deps)
    expect(handle).toHaveBeenCalledTimes(1)
    expect(handle.mock.calls[0][0]).toBe(IpcChannels.KnowledgeEntryAdmit)
    expect(guard).toHaveBeenCalledTimes(1)
  })
})
