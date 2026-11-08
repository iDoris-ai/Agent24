import { describe, expect, it, vi } from 'vitest'

import type { ChatHistoryMessage } from '../shared/chat-history'
import { MAX_ADMISSION_RUNS, admit, classify, replyRuns, strictest, type AdmissionReader, type FloorReading } from './knowledge-entry-admission'

const AT = '2026-11-09T12:00:00.000Z'
const user = (content: string): ChatHistoryMessage => ({ role: 'user', content, at: AT, delivered: true })
const reply = (content: string, run_id: string | null = null): ChatHistoryMessage => ({ role: 'assistant', content, at: AT, run_id, latency: null })
const record = (messages: ChatHistoryMessage[]) => ({ id: 'chat_abc', messages })

/** A daemon where the session floor and each run have a classification; read folds them. */
function daemon(floor: FloorReading, runs: Record<string, FloorReading> = {}) {
  const calls: Array<{ sessionId: string; runIds: readonly string[] }> = []
  const reader: AdmissionReader = {
    read: async (sessionId, runIds) => {
      calls.push({ sessionId, runIds })
      return strictest([floor, ...runIds.map((id) => runs[id] ?? 'unknown')])
    },
  }
  return { reader, calls }
}

describe('replyRuns', () => {
  it('lists the distinct run ids of replies only, in order', () => {
    expect(replyRuns(record([user('q'), reply('a', 'run_1'), user('q2'), reply('b'), reply('c', 'run_2'), reply('d', 'run_1')]))).toEqual(['run_1', 'run_2'])
    expect(replyRuns(record([user('q'), reply('a')]))).toEqual([])
  })
})

describe('strictest', () => {
  it('ranks restricted over unknown over missing over ordinary', () => {
    expect(strictest(['ordinary'])).toBe('ordinary')
    expect(strictest(['ordinary', 'missing'])).toBe('missing')
    expect(strictest(['missing', 'unknown', 'ordinary'])).toBe('unknown')
    expect(strictest(['unknown', 'restricted', 'ordinary'])).toBe('restricted')
  })

  it('never reads an empty list as ordinary', () => {
    expect(strictest([])).toBe('unknown')
  })
})

describe('classify (N10a, N10c–h)', () => {
  it('a /chat-only conversation is judged by its own floor (null run ids are the floor)', async () => {
    const d = daemon('ordinary')
    expect(await classify(record([user('q'), reply('a'), user('q2'), reply('b')]), d.reader)).toEqual({ ok: true })
    expect(d.calls).toEqual([{ sessionId: 'chat_abc', runIds: [] }])
  })

  it('refuses Restricted, Unknown and a missing floor, each with its own reason (N10c, N10d, N10e)', async () => {
    const conversation = record([user('q'), reply('a')])
    expect(await classify(conversation, daemon('restricted').reader)).toEqual({ ok: false, reason: 'restricted' })
    expect(await classify(conversation, daemon('unknown').reader)).toEqual({ ok: false, reason: 'unknown' })
    expect(await classify(conversation, daemon('missing').reader)).toEqual({ ok: false, reason: 'floor_missing' })
  })

  it('an Ordinary floor with one Restricted Knowledge run is refused (N10f)', async () => {
    const d = daemon('ordinary', { run_ok: 'ordinary', run_secret: 'restricted' })
    const conversation = record([user('q'), reply('a', 'run_ok'), user('q2'), reply('b', 'run_secret')])
    expect(await classify(conversation, d.reader)).toEqual({ ok: false, reason: 'restricted' })
    expect(d.calls[0].runIds).toEqual(['run_ok', 'run_secret'])
  })

  it('a run the daemon cannot find or read counts as Unknown (N10g)', async () => {
    const d = daemon('ordinary', { run_ok: 'ordinary' })
    expect(await classify(record([user('q'), reply('a', 'run_ok'), reply('b', 'run_gone')]), d.reader)).toEqual({ ok: false, reason: 'unknown' })
  })

  it('a reader that throws or answers something else refuses as Unknown', async () => {
    const conversation = record([user('q'), reply('a')])
    expect(await classify(conversation, { read: vi.fn(async () => { throw new Error('daemon down') }) })).toEqual({ ok: false, reason: 'unknown' })
    expect(await classify(conversation, { read: vi.fn(async () => 'public' as unknown as FloorReading) })).toEqual({ ok: false, reason: 'unknown' })
  })

  it('more runs than the limit are refused without asking, never cut short', async () => {
    const d = daemon('ordinary')
    const many = Array.from({ length: MAX_ADMISSION_RUNS + 1 }, (_, i) => reply(`a${i}`, `run_${i}`))
    expect(await classify(record([user('q'), ...many]), d.reader)).toEqual({ ok: false, reason: 'too_many_runs' })
    expect(d.calls).toEqual([])
    const exactly = many.slice(0, MAX_ADMISSION_RUNS)
    const all = Object.fromEntries(exactly.map((m) => [(m as { run_id: string }).run_id, 'ordinary' as const]))
    expect(await classify(record([user('q'), ...exactly]), daemon('ordinary', all).reader)).toEqual({ ok: true })
  })
})

describe('admit (N10a, N10b)', () => {
  it('needs both an Ordinary classification and an explicit confirmation', async () => {
    const conversation = record([user('q'), reply('a')])
    expect(await admit(conversation, daemon('ordinary').reader, true)).toEqual({ ok: true })
    expect(await admit(conversation, daemon('ordinary').reader, false)).toEqual({ ok: false, reason: 'not_confirmed' })
    expect(await admit(conversation, daemon('ordinary').reader, 'yes' as unknown as boolean)).toEqual({ ok: false, reason: 'not_confirmed' })
  })

  it('a refused classification wins over a confirmation', async () => {
    const conversation = record([user('q'), reply('a')])
    expect(await admit(conversation, daemon('restricted').reader, true)).toEqual({ ok: false, reason: 'restricted' })
    expect(await admit(conversation, daemon('missing').reader, true)).toEqual({ ok: false, reason: 'floor_missing' })
  })
})
