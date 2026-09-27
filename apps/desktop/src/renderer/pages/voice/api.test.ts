// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import {
  getAgentEarAttachment,
  getAgentEarUsage,
  genCommandId,
  isBuiltinProposal,
  sendSpeak,
  sendStopPlayback,
  totalCalls,
} from './api'
import type { AgentEarEventEnvelope } from '../../../shared/ipc-types'

type BackendReq = { method: string; path: string; body?: unknown }

function mockProxy(fn: (req: BackendReq) => Promise<{ ok: boolean; status: number; data: unknown }>): void {
  Object.defineProperty(window, 'agent24', {
    value: { backendProxy: vi.fn(fn) },
    writable: true,
    configurable: true,
  })
}

beforeEach(() => {
  vi.restoreAllMocks()
})

describe('genCommandId', () => {
  it('is unique across many consecutive calls', () => {
    const ids = new Set(Array.from({ length: 1000 }, () => genCommandId()))
    expect(ids.size).toBe(1000)
  })

  it('is a non-empty string prefixed cmd_', () => {
    const id = genCommandId()
    expect(id.startsWith('cmd_')).toBe(true)
    expect(id.length).toBeGreaterThan(4)
  })
})

describe('getAgentEarAttachment', () => {
  it('returns the agentear row filtered from GET /api/v1/attached', async () => {
    mockProxy((req) => {
      expect(req).toEqual({ method: 'GET', path: '/api/v1/attached' })
      return Promise.resolve({
        ok: true,
        status: 200,
        data: {
          modules: [
            { name: 'other', manifest_digest: 'x', token_id: 't', created_at: 'c', attach_status: 'attached' },
            { name: 'agentear', manifest_digest: 'y', token_id: 't2', created_at: 'c2', attach_status: 'attached' },
          ],
        },
      })
    })
    const view = await getAgentEarAttachment()
    expect(view?.name).toBe('agentear')
  })

  it('returns null when agentear has never registered', async () => {
    mockProxy(() => Promise.resolve({ ok: true, status: 200, data: { modules: [] } }))
    expect(await getAgentEarAttachment()).toBeNull()
  })

  it('throws with the error message on a non-ok response', async () => {
    mockProxy(() => Promise.resolve({ ok: false, status: 500, data: { error: { message: 'boom' } } }))
    await expect(getAgentEarAttachment()).rejects.toThrow('boom')
  })
})

describe('sendSpeak / sendStopPlayback — wire shape (design §6: POST /api/v1/os/{name}/commands/{command})', () => {
  it('speak posts schema/command_id/type/payload to the speak route', async () => {
    let captured: BackendReq | null = null
    mockProxy((req) => {
      captured = req
      return Promise.resolve({ ok: true, status: 200, data: { result: { accepted: true } } })
    })
    const id = await sendSpeak({ text: 'hi', lang: 'en-US' })
    expect(captured).not.toBeNull()
    expect(captured!.method).toBe('POST')
    expect(captured!.path).toBe('/api/v1/os/agentear/commands/speak')
    expect(captured!.body).toEqual({
      schema: 'agentear.command/1',
      command_id: id,
      type: 'speak',
      payload: { text: 'hi', lang: 'en-US' },
    })
  })

  it('stop_playback posts an empty payload by default and the given session_id when provided', async () => {
    let captured: BackendReq | null = null
    mockProxy((req) => {
      captured = req
      return Promise.resolve({ ok: true, status: 200, data: { result: { accepted: true } } })
    })
    await sendStopPlayback()
    expect(captured!.path).toBe('/api/v1/os/agentear/commands/stop_playback')
    expect((captured!.body as { payload: unknown }).payload).toEqual({})

    await sendStopPlayback('ses_1')
    expect((captured!.body as { payload: unknown }).payload).toEqual({ session_id: 'ses_1' })
  })

  it('two calls in a row use different command_ids', async () => {
    const ids: string[] = []
    mockProxy((req) => {
      ids.push((req.body as { command_id: string }).command_id)
      return Promise.resolve({ ok: true, status: 200, data: { result: { accepted: true } } })
    })
    await sendSpeak({ text: 'a', lang: 'zh-CN' })
    await sendSpeak({ text: 'b', lang: 'zh-CN' })
    expect(ids[0]).not.toBe(ids[1])
  })

  it('surfaces a 503 module_not_ready as an error', async () => {
    mockProxy(() => Promise.resolve({ ok: false, status: 503, data: { error: { message: 'module_not_ready' } } }))
    await expect(sendStopPlayback()).rejects.toThrow('module_not_ready')
  })
})

describe('isBuiltinProposal', () => {
  const base: AgentEarEventEnvelope = {
    schema: 'agentear.event/1', event_id: 'e', session_id: 's', seq: 1, type: 'proposal', payload: {},
  }

  it('true for action.type === "builtin"', () => {
    expect(isBuiltinProposal({ ...base, payload: { action: { type: 'builtin', name: 'style', value: 'yue' } } })).toBe(true)
  })

  it('false for a non-builtin action', () => {
    expect(isBuiltinProposal({ ...base, payload: { action: { type: 'open_url', url: 'mailto:x' } } })).toBe(false)
  })

  it('false for a non-proposal event even with a builtin-shaped payload', () => {
    expect(isBuiltinProposal({ ...base, type: 'turn', payload: { action: { type: 'builtin' } } })).toBe(false)
  })

  it('false when action is missing or malformed', () => {
    expect(isBuiltinProposal({ ...base, payload: {} })).toBe(false)
    expect(isBuiltinProposal({ ...base, payload: { action: 'builtin' } })).toBe(false)
  })
})

describe('getAgentEarUsage / totalCalls', () => {
  it('returns the by_served breakdown', async () => {
    mockProxy((req) => {
      expect(req.path).toBe('/api/v1/usage?module=agentear')
      return Promise.resolve({
        ok: true,
        status: 200,
        data: {
          module: 'agentear',
          totals: { calls_ok: 5, calls_failed: 0, calls_cancelled: 0, prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 },
          by_served: {
            local: { calls_ok: 4, calls_failed: 1, calls_cancelled: 0, prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 },
            remote: { calls_ok: 0, calls_failed: 0, calls_cancelled: 0, prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 },
            none: { calls_ok: 0, calls_failed: 0, calls_cancelled: 0, prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 },
          },
          daily: [],
          cost_usd: null,
        },
      })
    })
    const by = await getAgentEarUsage()
    expect(totalCalls(by!.local)).toBe(5)
    expect(totalCalls(by!.remote)).toBe(0)
  })
})
