// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import VoicePanel, { MAX_EVENTS_IN_MEMORY, pushCapped } from './VoicePanel'
import type { AgentEarEventEnvelope, AttachedView } from '../../../shared/ipc-types'

type BackendReq = { method: string; path: string; body?: unknown }
type BackendRes = { ok: boolean; status: number; data: unknown }

let posted: Array<{ path: string; body: unknown }> = []
let onEventHandler: ((raw: unknown) => void) | null = null
let attachModules: AttachedView[] = []
let snapshotEvents: AgentEarEventEnvelope[] = []

function mount(): void {
  posted = []
  onEventHandler = null
  const proxy = vi.fn((req: BackendReq): Promise<BackendRes> => {
    if (req.method === 'GET' && req.path === '/api/v1/attached') {
      return Promise.resolve({ ok: true, status: 200, data: { modules: attachModules } })
    }
    if (req.method === 'GET' && req.path.startsWith('/api/v1/usage')) {
      return Promise.resolve({
        ok: true,
        status: 200,
        data: {
          module: 'agentear',
          totals: { calls_ok: 0, calls_failed: 0, calls_cancelled: 0, prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 },
          by_served: {
            local: { calls_ok: 3, calls_failed: 0, calls_cancelled: 0, prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 },
            remote: { calls_ok: 0, calls_failed: 0, calls_cancelled: 0, prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 },
            none: { calls_ok: 0, calls_failed: 0, calls_cancelled: 0, prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 },
          },
          daily: [],
          cost_usd: null,
        },
      })
    }
    if (req.method === 'POST' && req.path.startsWith('/api/v1/os/agentear/commands/')) {
      posted.push({ path: req.path, body: req.body })
      return Promise.resolve({ ok: true, status: 200, data: { result: { accepted: true } } })
    }
    return Promise.resolve({ ok: false, status: 404, data: { error: { message: 'unhandled in test mock' } } })
  })

  const onAgentEarEvent = vi.fn((cb: (raw: unknown) => void) => {
    onEventHandler = cb
    return () => { onEventHandler = null }
  })

  // Review M5: the log (sequencing + 200-cap) now lives in main
  // (agentear-log.ts) — the panel just pulls whatever it already holds once
  // on mount, so this mock stands in for main's `agentear:snapshot` handler.
  const agentearSnapshot = vi.fn(() => Promise.resolve(snapshotEvents))

  Object.defineProperty(window, 'agent24', {
    value: { backendProxy: proxy, onAgentEarEvent, agentearSnapshot },
    writable: true,
    configurable: true,
  })
}

function attached(): AttachedView {
  return {
    name: 'agentear',
    manifest_digest: 'sha256:deadbeef',
    token_id: 'tok_1',
    created_at: '2026-09-27T00:00:00Z',
    attach_status: 'attached',
    generation: 3,
  }
}

function emit(ev: AgentEarEventEnvelope): void {
  expect(onEventHandler).not.toBeNull()
  onEventHandler!(ev)
}

function turn(session: string, seq: number, phase = 'listening'): AgentEarEventEnvelope {
  return { schema: 'agentear.event/1', event_id: `evt_${session}_${seq}`, session_id: session, seq, type: 'turn', payload: { phase } }
}

function transcript(session: string, seq: number, text: string): AgentEarEventEnvelope {
  return { schema: 'agentear.event/1', event_id: `evt_${session}_${seq}`, session_id: session, seq, type: 'transcript', payload: { text, lang: 'zh-CN', final: true } }
}

beforeEach(() => {
  attachModules = []
  snapshotEvents = []
})

describe('VoicePanel — attach status', () => {
  it('disables speak input and both buttons when the module is detached (no record)', async () => {
    mount()
    render(<VoicePanel />)
    await waitFor(() => expect(screen.getByText(/未附着/)).toBeInTheDocument())
    expect(screen.getByLabelText('让它说')).toBeDisabled()
    expect(screen.getByRole('button', { name: '让它说' })).toBeDisabled()
    expect(screen.getByRole('button', { name: '停止播放' })).toBeDisabled()
  })

  it('disables everything when attach_status is explicitly detached', async () => {
    attachModules = [{ ...attached(), attach_status: 'detached' }]
    mount()
    render(<VoicePanel />)
    await waitFor(() => expect(screen.getByText(/未附着/)).toBeInTheDocument())
    expect(screen.getByRole('button', { name: '让它说' })).toBeDisabled()
    expect(screen.getByRole('button', { name: '停止播放' })).toBeDisabled()
  })

  it('enables the panel when attached', async () => {
    attachModules = [attached()]
    mount()
    render(<VoicePanel />)
    await waitFor(() => expect(screen.getByText(/已附着/)).toBeInTheDocument())
    expect(screen.getByLabelText('让它说')).not.toBeDisabled()
    expect(screen.getByRole('button', { name: '停止播放' })).not.toBeDisabled()
    // speak button stays disabled until there's text
    expect(screen.getByRole('button', { name: '让它说' })).toBeDisabled()
  })

  it('caps the speak input at 2000 chars (agentear.command.v1 schema: speak.text maxLength)', async () => {
    attachModules = [attached()]
    mount()
    render(<VoicePanel />)
    await waitFor(() => expect(screen.getByText(/已附着/)).toBeInTheDocument())
    expect(screen.getByLabelText('让它说')).toHaveAttribute('maxLength', '2000')
  })
})

describe('VoicePanel — speak / stop commands', () => {
  beforeEach(() => {
    attachModules = [attached()]
  })

  it('speak posts the correct URL and an agentear.command/1 body shape', async () => {
    mount()
    render(<VoicePanel />)
    await waitFor(() => expect(screen.getByText(/已附着/)).toBeInTheDocument())
    fireEvent.change(screen.getByLabelText('让它说'), { target: { value: '你好' } })
    fireEvent.click(screen.getByRole('button', { name: '让它说' }))
    await waitFor(() => expect(posted).toHaveLength(1))
    expect(posted[0]!.path).toBe('/api/v1/os/agentear/commands/speak')
    const body = posted[0]!.body as Record<string, unknown>
    expect(body['schema']).toBe('agentear.command/1')
    expect(body['type']).toBe('speak')
    expect(typeof body['command_id']).toBe('string')
    expect((body['command_id'] as string).length).toBeGreaterThan(0)
    expect(body['payload']).toEqual({ text: '你好', lang: 'zh-CN' })
  })

  it('stop_playback posts the correct URL and body shape', async () => {
    mount()
    render(<VoicePanel />)
    await waitFor(() => screen.getByRole('button', { name: '停止播放' }))
    fireEvent.click(screen.getByRole('button', { name: '停止播放' }))
    await waitFor(() => expect(posted).toHaveLength(1))
    expect(posted[0]!.path).toBe('/api/v1/os/agentear/commands/stop_playback')
    const body = posted[0]!.body as Record<string, unknown>
    expect(body['schema']).toBe('agentear.command/1')
    expect(body['type']).toBe('stop_playback')
    expect(typeof body['command_id']).toBe('string')
  })

  it('each command gets a unique command_id', async () => {
    mount()
    render(<VoicePanel />)
    await waitFor(() => screen.getByRole('button', { name: '停止播放' }))
    fireEvent.click(screen.getByRole('button', { name: '停止播放' }))
    fireEvent.click(screen.getByRole('button', { name: '停止播放' }))
    await waitFor(() => expect(posted).toHaveLength(2))
    const ids = posted.map((p) => (p.body as Record<string, unknown>)['command_id'])
    expect(new Set(ids).size).toBe(2)
  })
})

describe('VoicePanel — live event display', () => {
  beforeEach(() => {
    attachModules = [attached()]
  })

  it('shows a transcript event with its language', async () => {
    mount()
    render(<VoicePanel />)
    await waitFor(() => expect(onEventHandler).not.toBeNull())
    emit(transcript('ses_1', 1, '今天天气怎么样？'))
    await waitFor(() => expect(screen.getByText('今天天气怎么样？')).toBeInTheDocument())
    expect(screen.getByText(/转写 · zh-CN/)).toBeInTheDocument()
  })

  it('shows a truncation annotation for a transcript longer than the display limit', async () => {
    mount()
    render(<VoicePanel />)
    await waitFor(() => expect(onEventHandler).not.toBeNull())
    const longText = '啊'.repeat(250)
    emit(transcript('ses_1', 1, longText))
    await waitFor(() => expect(screen.getByText(/已截断/)).toBeInTheDocument())
    expect(screen.getByText(/共 250 字/)).toBeInTheDocument()
  })

  it('does not annotate a short transcript as truncated', async () => {
    mount()
    render(<VoicePanel />)
    await waitFor(() => expect(onEventHandler).not.toBeNull())
    emit(transcript('ses_1', 1, '你好'))
    await waitFor(() => expect(screen.getByText('你好')).toBeInTheDocument())
    expect(screen.queryByText(/已截断/)).not.toBeInTheDocument()
  })

  it('a builtin proposal is labeled as already-executed locally, with no execute button', async () => {
    mount()
    render(<VoicePanel />)
    await waitFor(() => expect(onEventHandler).not.toBeNull())
    emit({
      schema: 'agentear.event/1',
      event_id: 'evt_p1',
      session_id: 'ses_1',
      seq: 1,
      type: 'proposal',
      payload: {
        schema: 'agentear.proposal/1',
        text: '说粤语',
        normalized: '说粤语',
        matched: '说粤语',
        rest: '',
        action: { type: 'builtin', name: 'style', value: 'yue' },
        needs_confirm: false,
        prompt: null,
        command_count: 12,
        commands_path: '/x',
      },
    })
    await waitFor(() => expect(screen.getByText('已在 AgentEar 本地执行')).toBeInTheDocument())
    // no execute/confirm affordance is ever offered for a proposal row (P2 scope, §7.1/§10)
    expect(screen.queryByRole('button', { name: /执行/ })).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /确认/ })).not.toBeInTheDocument()
  })

  it('a non-builtin proposal is labeled "P3 前不执行", also with no execute button', async () => {
    mount()
    render(<VoicePanel />)
    await waitFor(() => expect(onEventHandler).not.toBeNull())
    emit({
      schema: 'agentear.event/1',
      event_id: 'evt_p2',
      session_id: 'ses_1',
      seq: 1,
      type: 'proposal',
      payload: {
        schema: 'agentear.proposal/1',
        text: '发邮件给 a@b.com',
        normalized: '发邮件给abcom',
        matched: '发邮件给',
        rest: 'a@b.com',
        action: { type: 'open_url', url: 'mailto:{rest}' },
        needs_confirm: true,
        prompt: '确认？',
        command_count: 12,
        commands_path: '/x',
      },
    })
    await waitFor(() => expect(screen.getByText('P3 前不执行')).toBeInTheDocument())
    expect(screen.queryByRole('button', { name: /执行/ })).not.toBeInTheDocument()
  })

  it('caps the in-memory event log at MAX_EVENTS_IN_MEMORY, dropping the oldest', async () => {
    mount()
    render(<VoicePanel />)
    await waitFor(() => expect(onEventHandler).not.toBeNull())
    const total = MAX_EVENTS_IN_MEMORY + 5
    for (let i = 1; i <= total; i++) {
      emit(transcript('ses_cap', i, `msg-${i}`))
    }
    await waitFor(() => expect(screen.getByText('msg-' + total)).toBeInTheDocument())
    expect(screen.queryByText('msg-1')).not.toBeInTheDocument()
    expect(screen.queryByText('msg-5')).not.toBeInTheDocument()
    expect(screen.getByText('msg-6')).toBeInTheDocument() // oldest surviving row
    expect(document.querySelectorAll('.voice-row')).toHaveLength(MAX_EVENTS_IN_MEMORY)
  })

  it('review M5: remounting the panel (navigating away and back) does not lose the list — it comes from main\'s snapshot', async () => {
    // Simulate main (agentear-log.ts) already holding events from BEFORE
    // this mount — e.g. because the user was on another sidebar page while
    // AgentEar kept talking, or simply switched to 语音 and back.
    snapshotEvents = [transcript('ses_1', 1, '早于挂载就已经在主进程日志里')]
    mount()
    const { unmount } = render(<VoicePanel />)
    await waitFor(() => expect(screen.getByText('早于挂载就已经在主进程日志里')).toBeInTheDocument())

    // Unmount (as App.tsx does when navigating to another page) and remount —
    // the OLD architecture (sequencer + state living in this component) would
    // show an empty panel here; the fix must not.
    unmount()
    render(<VoicePanel />)
    await waitFor(() => expect(screen.getByText('早于挂载就已经在主进程日志里')).toBeInTheDocument())
  })
})

describe('pushCapped — ring buffer unit', () => {
  const mk = (seq: number): AgentEarEventEnvelope => ({
    schema: 'agentear.event/1', event_id: `e${seq}`, session_id: 's', seq, type: 'turn', payload: {},
  })

  it('keeps only the most recent `cap` items', () => {
    let acc: AgentEarEventEnvelope[] = []
    for (let i = 1; i <= 10; i++) acc = pushCapped(acc, [mk(i)], 4)
    expect(acc.map((e) => e.seq)).toEqual([7, 8, 9, 10])
  })

  it('is a no-op for an empty batch', () => {
    const acc = [mk(1)]
    expect(pushCapped(acc, [], 4)).toBe(acc)
  })
})

describe('VoicePanel — no persistence (design §7.3/Q4)', () => {
  it('never writes to localStorage or sessionStorage across a full interaction', async () => {
    attachModules = [attached()]
    mount()
    const localSpy = vi.spyOn(Storage.prototype, 'setItem')
    render(<VoicePanel />)
    await waitFor(() => expect(onEventHandler).not.toBeNull())

    emit(transcript('ses_1', 1, '你好'))
    await waitFor(() => expect(screen.getByText('你好')).toBeInTheDocument())

    fireEvent.change(screen.getByLabelText('让它说'), { target: { value: 'hi' } })
    fireEvent.click(screen.getByRole('button', { name: '让它说' }))
    await waitFor(() => expect(posted.length).toBeGreaterThan(0))

    fireEvent.click(screen.getByRole('button', { name: '停止播放' }))
    await waitFor(() => expect(posted.length).toBeGreaterThan(1))

    expect(localSpy).not.toHaveBeenCalled()
    localSpy.mockRestore()
  })
})
