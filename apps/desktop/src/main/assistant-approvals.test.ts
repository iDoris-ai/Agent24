import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createChatHistoryStore, onChatHistoryChange } from './chat-history'
import { ASSISTANT_APPROVALS_FORMAT, ASSISTANT_APPROVALS_VERSION, createAssistantApprovals, type RunOwner } from './assistant-approvals'

// M4 A6: approvals of runs no conversation asked for go to the assistant's
// conversation, against the real chat history store.

let dir: string
let history: ReturnType<typeof createChatHistoryStore>
let owners: Record<string, RunOwner>
let ownerOf: ReturnType<typeof vi.fn>
const approvals = (extra: Partial<Parameters<typeof createAssistantApprovals>[0]> = {}) => createAssistantApprovals({
  dir, history, ownerOf, title: () => '助理发起', ...extra,
})
const pending = (...runs: unknown[]) => async () => runs.map((run_id, i) => ({ id: `apr_${i}`, run_id }))
beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'assistant-approvals-'))
  history = createChatHistoryStore(dir)
  owners = { run_cli: 'other', run_cli2: 'other', run_sch: 'schedule', run_desk: 'desktop' }
  ownerOf = vi.fn(async (id: string) => {
    if (!(id in owners)) throw new Error('backend_not_ready')
    return owners[id]
  })
})
afterEach(() => { onChatHistoryChange(null); vi.restoreAllMocks(); fs.rmSync(dir, { recursive: true, force: true }) })

describe('assistant approvals (M4 A6)', () => {
  it('records the run of an approval no conversation has as a task of the assistant\'s conversation, and the window hears of it', async () => {
    const told = vi.fn()
    onChatHistoryChange(told)
    const summary = await approvals().route('run_cli')
    expect(summary).toMatchObject({ title: '助理发起', title_source: 'assistant', tasks: [{ run_id: 'run_cli', kind: 'task' }] })
    expect(history.list()).toHaveLength(1)
    expect(told).toHaveBeenLastCalledWith(expect.objectContaining({ id: summary!.id }))
  })

  it('puts later runs in the same conversation, also after the app starts again', async () => {
    const first = await approvals().route('run_cli')
    const again = await approvals().route('run_cli2')
    expect(again!.id).toBe(first!.id)
    expect(again!.tasks.map(t => t.run_id)).toEqual(['run_cli', 'run_cli2'])
    expect(history.list()).toHaveLength(1)
  })

  it.each(['run_sch', 'run_desk'])('leaves %s to the conversation that has its card (a schedule\'s, or the page\'s), and asks of it no more', async (run) => {
    const m = approvals()
    expect(await m.route(run)).toBeNull()
    expect(history.list()).toHaveLength(0)
    await m.route(run)
    expect(ownerOf).toHaveBeenCalledTimes(1)
  })

  it('records a run once, for a second approval of it too', async () => {
    const m = approvals()
    await m.route('run_cli')
    expect(await m.route('run_cli')).toBeNull()
    expect(history.list()[0]!.tasks).toHaveLength(1)
    expect(ownerOf).toHaveBeenCalledTimes(1)
  })

  it('starts a new conversation when the assistant\'s was deleted, for a run not handled yet; not for one handled', async () => {
    const first = await approvals().route('run_cli')
    history.delete(first!.id)
    expect(await approvals().route('run_cli')).toBeNull()
    expect(history.list()).toHaveLength(0)
    const next = await approvals().route('run_cli2')
    expect(next!.id).not.toBe(first!.id)
    expect(next!.tasks.map(t => t.run_id)).toEqual(['run_cli2'])
  })

  it('starts a new conversation when the assistant\'s is full, and puts the runs from then on there (Codex A6 C6)', async () => {
    const first = await approvals().route('run_cli')
    const full = { ...history, appendRun: vi.fn((id: unknown, runId: unknown, kind: unknown) => {
      if (id === first!.id) throw new Error('Too many chat history runs')
      return history.appendRun(id, runId, kind)
    }) }
    const next = await approvals({ history: full }).route('run_cli2')
    expect(next!.id).not.toBe(first!.id)
    expect(next).toMatchObject({ title: '助理发起', title_source: 'assistant', tasks: [{ run_id: 'run_cli2' }] })
    owners.run_cli3 = 'other'
    expect((await approvals({ history: full }).route('run_cli3'))!.id).toBe(next!.id)
  })

  it('does not start a new conversation for another failure to record', async () => {
    await approvals().route('run_cli')
    const broken = { ...history, appendRun: vi.fn(() => { throw new Error('disk full') }) }
    await expect(approvals({ history: broken }).route('run_cli2')).rejects.toThrow('disk full')
    expect(history.list()).toHaveLength(1)
  })

  it('tells A5 whether a run it took was recorded, and only of runs it took (Codex A6 r3 C2)', async () => {
    const settled = vi.fn(async () => {})
    await approvals({ settled }).route('run_cli')
    await approvals({ settled }).route('run_sch')
    await approvals({ settled }).route('run_desk')
    const broken = { ...history, appendRun: vi.fn(() => { throw new Error('disk full') }) }
    await expect(approvals({ settled, history: broken }).route('run_cli2')).rejects.toThrow('disk full')
    expect(settled.mock.calls).toEqual([['run_cli', true], ['run_cli2', false]])
  })

  it('keeps a recorded run handled even when A5 cannot be told', async () => {
    const settled = vi.fn(async () => { throw new Error('disk full') })
    expect(await approvals({ settled }).route('run_cli')).not.toBeNull()
    expect(await approvals().route('run_cli')).toBeNull()
    expect(settled.mock.calls).toEqual([['run_cli', true]])
  })

  it('routes two runs at once into one new conversation, one at a time', async () => {
    const m = approvals()
    const [a, b] = await Promise.all([m.route('run_cli'), m.route('run_cli2')])
    expect(a!.id).toBe(b!.id)
    expect(history.list()).toHaveLength(1)
  })

  it('routes runs in the order heard of, whichever the daemon answers first', async () => {
    let first!: () => void
    ownerOf.mockImplementationOnce(() => new Promise<RunOwner>(resolve => { first = () => resolve('other') }))
    const m = approvals()
    const a = m.route('run_cli')
    const b = m.route('run_cli2')
    await vi.waitFor(() => expect(first).toBeTypeOf('function'))
    first()
    await Promise.all([a, b])
    expect(history.list()[0]!.tasks.map(t => t.run_id)).toEqual(['run_cli', 'run_cli2'])
  })

  it('does not take a run as handled when the daemon cannot tell whose it is: the next catch-up routes it', async () => {
    const m = approvals()
    await expect(m.route('run_new')).rejects.toThrow('backend_not_ready')
    expect(history.list()).toHaveLength(0)
    owners.run_new = 'other'
    await m.catchUp(pending('run_new'))
    expect(history.list()[0]!.tasks.map(t => t.run_id)).toEqual(['run_new'])
  })

  it('does not take a run as handled when it could not be recorded', async () => {
    const appendRun = vi.fn(() => { throw new Error('disk full') })
    const m = approvals({ history: { ...history, get: history.get, create: history.create, appendRun } })
    await expect(m.route('run_cli')).rejects.toThrow('disk full')
    await approvals().route('run_cli')
    expect(history.list().flatMap(s => s.tasks.map(t => t.run_id))).toEqual(['run_cli'])
  })

  describe('catch-up on connecting', () => {
    it('routes the pending approvals\' runs, the first time too (an approval from before has nowhere else to be decided)', async () => {
      await approvals().catchUp(pending('run_cli', 'run_sch', 'run_desk', 'run_cli2'))
      const list = history.list()
      expect(list).toHaveLength(1)
      expect(list[0]!.tasks.map(t => t.run_id)).toEqual(['run_cli', 'run_cli2'])
    })

    it.each([['the event first', 'live'], ['the catch-up first', 'catchUp']])('gives a run one card, heard of by the event and the catch-up, %s (QA ⑬)', async (_, first) => {
      const m = approvals()
      const live = () => m.route('run_cli')
      const catchUp = () => m.catchUp(pending('run_cli'))
      if (first === 'live') { await live(); await catchUp() } else { await catchUp(); await live() }
      const list = history.list()
      expect(list).toHaveLength(1)
      expect(list[0]!.tasks.map(t => t.run_id)).toEqual(['run_cli'])
      expect(ownerOf).toHaveBeenCalledTimes(1)
    })

    it('gives no card to a run the window asked, waiting for approval when the window opens again: the page is to promote it (QA N6, known limit)', async () => {
      await approvals().catchUp(pending('run_desk'))
      expect(history.list()).toHaveLength(0)
    })

    it('routes a run with two pending approvals once', async () => {
      await approvals().catchUp(pending('run_cli', 'run_cli'))
      expect(ownerOf).toHaveBeenCalledTimes(1)
      expect(history.list()[0]!.tasks).toHaveLength(1)
    })

    it('does not route again what was handled, live or at an earlier catch-up', async () => {
      await approvals().route('run_cli')
      await approvals().catchUp(pending('run_cli', 'run_sch'))
      await approvals().catchUp(pending('run_cli', 'run_sch'))
      expect(ownerOf).toHaveBeenCalledTimes(2)
      expect(history.list()[0]!.tasks).toHaveLength(1)
    })

    it('goes on past a run it cannot route, tells of it, and tries it again next time', async () => {
      const onCatchUpFailure = vi.fn()
      await approvals({ onCatchUpFailure }).catchUp(pending('run_gone', 'run_cli'))
      expect(onCatchUpFailure).toHaveBeenCalledWith('run_gone', expect.any(Error))
      expect(history.list()[0]!.tasks.map(t => t.run_id)).toEqual(['run_cli'])
      owners.run_gone = 'other'
      await approvals().catchUp(pending('run_gone', 'run_cli'))
      expect(history.list()[0]!.tasks.map(t => t.run_id)).toEqual(['run_cli', 'run_gone'])
    })

    it.each([[null], ['bad id!'], [42], [undefined]])('skips an approval whose run id is %j', async (bad) => {
      await approvals().catchUp(pending(bad, 'run_cli'))
      expect(ownerOf).toHaveBeenCalledTimes(1)
      expect(ownerOf).toHaveBeenCalledWith('run_cli')
    })

    it('changes nothing when the approvals cannot be had', async () => {
      await expect(approvals().catchUp(async () => { throw new Error('approvals_unavailable_503') })).rejects.toThrow()
      expect(history.list()).toHaveLength(0)
      expect(fs.existsSync(path.join(dir, 'assistant-approvals.json'))).toBe(false)
    })
  })

  describe('the file', () => {
    const file = () => path.join(dir, 'assistant-approvals.json')

    it('refuses ids that are not the daemon\'s, and goes on after a failure', async () => {
      const m = approvals()
      await expect(m.route('../x')).rejects.toThrow('Invalid run')
      await expect(m.route('run_x')).rejects.toThrow()
      expect(await m.route('run_cli')).not.toBeNull()
    })

    it('starts over from a file that is not its own: a new conversation, nothing handled', async () => {
      const first = await approvals().route('run_cli')
      fs.writeFileSync(file(), JSON.stringify({ format: 'other', version: ASSISTANT_APPROVALS_VERSION, conversation: first!.id, handled: [] }))
      await approvals().route('run_cli2')
      expect(history.list()).toHaveLength(2)
      fs.writeFileSync(file(), '{not json')
      expect(await approvals().route('run_cli')).not.toBeNull()
      expect(history.list()).toHaveLength(3)
    })

    it('drops a conversation id that is not one, and handled ids that are not', async () => {
      fs.writeFileSync(file(), JSON.stringify({
        format: ASSISTANT_APPROVALS_FORMAT, version: ASSISTANT_APPROVALS_VERSION, conversation: '../../etc', handled: ['run_cli', 7, '../x'],
      }))
      expect(await approvals().route('run_cli')).toBeNull()
      expect(await approvals().route('run_cli2')).not.toBeNull()
      expect(JSON.parse(fs.readFileSync(file(), 'utf8')).handled).toEqual(['run_cli', 'run_cli2'])
    })

    it('keeps the newest 5000 handled runs', async () => {
      const ids = Array.from({ length: 5000 }, (_, i) => `run_${i}`)
      fs.writeFileSync(file(), JSON.stringify({ format: ASSISTANT_APPROVALS_FORMAT, version: ASSISTANT_APPROVALS_VERSION, conversation: null, handled: ids }))
      await approvals().route('run_cli')
      const handled = JSON.parse(fs.readFileSync(file(), 'utf8')).handled as string[]
      expect(handled).toHaveLength(5000)
      expect(handled[0]).toBe('run_1')
      expect(handled.at(-1)).toBe('run_cli')
    })
  })
})
