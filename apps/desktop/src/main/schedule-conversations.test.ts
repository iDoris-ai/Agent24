import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createChatHistoryStore, onChatHistoryChange } from './chat-history'
import { createScheduleConversations, SCHEDULE_CONVERSATIONS_FORMAT, SCHEDULE_CONVERSATIONS_VERSION } from './schedule-conversations'

// M4 A5: a schedule's runs go to the schedule's conversation, against the
// real chat history store.

let dir: string
let history: ReturnType<typeof createChatHistoryStore>
let names: Record<string, string>
let scheduleName: ReturnType<typeof vi.fn>
const homes = () => createScheduleConversations({
  dir, history, scheduleName, title: name => `日程：${name}`,
})
beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'schedule-conversations-'))
  history = createChatHistoryStore(dir)
  names = { sch_1: '每日周报', sch_2: '月底对账' }
  scheduleName = vi.fn(async (id: string) => names[id] ?? null)
})
afterEach(() => { onChatHistoryChange(null); vi.restoreAllMocks(); fs.rmSync(dir, { recursive: true, force: true }) })

describe('schedule conversations (M4 A5)', () => {
  it('starts a conversation named after the schedule at its first run, and records its runs there as the schedule\'s', async () => {
    const told = vi.fn()
    onChatHistoryChange(told)
    const summary = await homes().record('sch_1', 'run_1')
    expect(summary).toMatchObject({ title: '日程：每日周报', title_source: 'schedule', tasks: [{ run_id: 'run_1', kind: 'schedule' }] })
    expect(history.list()).toHaveLength(1)
    // The window hears of the new conversation, then of its run.
    expect(told).toHaveBeenCalledTimes(2)
    expect(told).toHaveBeenLastCalledWith(expect.objectContaining({ id: summary!.id, tasks: [expect.objectContaining({ run_id: 'run_1' })] }))
  })

  it('records later runs of the schedule in the same conversation, also after the app starts again', async () => {
    const first = await homes().record('sch_1', 'run_1')
    // main starts again: a new module, the same file.
    const again = await homes().record('sch_1', 'run_2')
    expect(again!.id).toBe(first!.id)
    expect(again!.tasks.map(t => t.run_id)).toEqual(['run_1', 'run_2'])
    expect(history.list()).toHaveLength(1)
    expect(scheduleName).toHaveBeenCalledTimes(1)
    expect(homes().conversationOf('sch_1')).toBe(first!.id)
  })

  it('gives each schedule its own conversation', async () => {
    const a = await homes().record('sch_1', 'run_1')
    const b = await homes().record('sch_2', 'run_2')
    expect(a!.id).not.toBe(b!.id)
    expect(history.list().map(s => s.title).sort()).toEqual(['日程：月底对账', '日程：每日周报'])
  })

  it('records two runs of a new schedule in one new conversation, one at a time', async () => {
    const m = homes()
    const [a, b] = await Promise.all([m.record('sch_1', 'run_1'), m.record('sch_1', 'run_2')])
    expect(a!.id).toBe(b!.id)
    expect(history.list()).toHaveLength(1)
    expect(history.get(a!.id)!.runs.map(r => r.run_id)).toEqual(['run_1', 'run_2'])
  })

  it('starts a new conversation for a schedule whose conversation was deleted', async () => {
    const first = await homes().record('sch_1', 'run_1')
    history.delete(first!.id)
    expect(homes().conversationOf('sch_1')).toBeNull()
    const next = await homes().record('sch_1', 'run_2')
    expect(next!.id).not.toBe(first!.id)
    expect(next!.tasks.map(t => t.run_id)).toEqual(['run_2'])
  })

  it('keeps recording to an archived conversation: it is still the schedule\'s', async () => {
    const first = await homes().record('sch_1', 'run_1')
    history.organize(first!.id, { archived: true })
    const next = await homes().record('sch_1', 'run_2')
    expect(next!.id).toBe(first!.id)
  })

  it('names the conversation after the schedule\'s id when the daemon cannot tell its name', async () => {
    scheduleName.mockRejectedValueOnce(new Error('backend_not_ready'))
    expect(await homes().record('sch_1', 'run_1')).toMatchObject({ title: '日程：sch_1' })
    expect(await homes().record('sch_9', 'run_2')).toMatchObject({ title: '日程：sch_9' })
  })

  it('starts over from a file it cannot read, and keeps nothing it does not understand', async () => {
    fs.writeFileSync(path.join(dir, 'schedule-conversations.json'), '{not json')
    const first = await homes().record('sch_1', 'run_1')
    expect(first).toMatchObject({ title: '日程：每日周报' })
    fs.writeFileSync(path.join(dir, 'schedule-conversations.json'), JSON.stringify({
      format: SCHEDULE_CONVERSATIONS_FORMAT, version: SCHEDULE_CONVERSATIONS_VERSION,
      bindings: { sch_1: first!.id, 'bad id': first!.id, sch_2: '../etc/passwd' },
    }))
    expect(homes().conversationOf('sch_1')).toBe(first!.id)
    expect(homes().conversationOf('sch_2')).toBeNull()
    fs.writeFileSync(path.join(dir, 'schedule-conversations.json'), JSON.stringify({ format: 'other', version: 1, bindings: { sch_1: first!.id } }))
    expect(homes().conversationOf('sch_1')).toBeNull()
  })

  it('refuses ids that are not the daemon\'s, and goes on after a failure', async () => {
    const m = homes()
    await expect(m.record('../x', 'run_1')).rejects.toThrow('Invalid schedule run')
    await expect(m.record('sch_1', 'run 1')).rejects.toThrow('Invalid schedule run')
    expect(history.list()).toHaveLength(0)
    scheduleName.mockImplementationOnce(async () => { throw new Error('x') })
    vi.spyOn(history, 'create').mockImplementationOnce(() => { throw new Error('disk full') })
    await expect(m.record('sch_1', 'run_1')).rejects.toThrow('disk full')
    expect(await m.record('sch_1', 'run_2')).toMatchObject({ tasks: [{ run_id: 'run_2', kind: 'schedule' }] })
  })

  describe('catching up on runs started while main was not listening (Codex A5 C1)', () => {
    const file = () => JSON.parse(fs.readFileSync(path.join(dir, 'schedule-conversations.json'), 'utf8'))
    const row = (id: string, schedule: string | null, minutesFromNow: number) =>
      ({ id, schedule_id: schedule, created_at: new Date(Date.now() + minutesFromNow * 60_000).toISOString() })

    it('only notes the time the first time: there is nothing to catch up on yet', async () => {
      const runs = vi.fn(async () => [row('run_old', 'sch_1', -60)])
      await homes().catchUp(runs)
      expect(runs).not.toHaveBeenCalled()
      expect(history.list()).toHaveLength(0)
      expect(Date.parse(file().synced_at)).toBeGreaterThan(Date.now() - 5_000)
    })

    it('records the schedules\' runs missed since the last one heard of, oldest first, each once', async () => {
      const m = homes()
      await m.record('sch_1', 'run_1')
      // Cut off; meanwhile sch_1 runs again and a new schedule runs twice.
      const missed = [row('run_3', 'sch_2', 2), row('run_1', 'sch_1', 0), row('run_chat', null, 1), row('run_2', 'sch_2', 1), row('run_4', 'sch_1', 3)]
      await m.catchUp(async () => missed)
      const [one] = history.list().filter(s => s.title === '日程：每日周报')
      const [two] = history.list().filter(s => s.title === '日程：月底对账')
      expect(history.list()).toHaveLength(2)
      expect(history.get(one.id)!.runs.map(r => r.run_id)).toEqual(['run_1', 'run_4'])
      expect(history.get(two.id)!.runs.map(r => r.run_id)).toEqual(['run_2', 'run_3'])
      // Caught up: the same runs again add nothing.
      await m.catchUp(async () => missed)
      expect(history.get(one.id)!.runs).toHaveLength(2)
    })

    it('leaves runs from before the last one heard of alone, so a deleted conversation is not brought back', async () => {
      vi.useFakeTimers({ toFake: ['Date'] })
      try {
        const m = homes()
        vi.setSystemTime(new Date('2026-11-09T10:00:00Z'))
        await m.catchUp(async () => [])
        // Heard of live an hour later; then the user deletes the conversation.
        vi.setSystemTime(new Date('2026-11-09T11:00:00Z'))
        await m.record('sch_1', 'run_1')
        history.delete(history.list()[0].id)
        vi.setSystemTime(new Date('2026-11-09T12:00:00Z'))
        await m.catchUp(async () => [{ id: 'run_0', schedule_id: 'sch_1', created_at: '2026-11-09T10:30:00Z' }])
        expect(history.list()).toHaveLength(0)
      } finally {
        vi.useRealTimers()
      }
    })

    it('tries the same span again after a catch-up that could not ask the daemon', async () => {
      vi.useFakeTimers({ toFake: ['Date'] })
      try {
        const m = homes()
        vi.setSystemTime(new Date('2026-11-09T10:00:00Z'))
        await m.catchUp(async () => [])
        const synced = file().synced_at
        vi.setSystemTime(new Date('2026-11-09T11:00:00Z'))
        await expect(m.catchUp(async () => { throw new Error('runs_unavailable_503') })).rejects.toThrow('runs_unavailable_503')
        expect(file().synced_at).toBe(synced)
        vi.setSystemTime(new Date('2026-11-09T12:00:00Z'))
        await m.catchUp(async () => [{ id: 'run_1', schedule_id: 'sch_1', created_at: '2026-11-09T10:30:00Z' }])
        expect(history.list()).toHaveLength(1)
      } finally {
        vi.useRealTimers()
      }
    })

    it('skips rows that are not runs', async () => {
      const m = homes()
      await m.catchUp(async () => [])
      const rows = [null, 7, { id: 'run_1' }, { id: 'run 2', schedule_id: 'sch_1', created_at: new Date().toISOString() },
        { id: 'run_3', schedule_id: 'sch_1', created_at: 'not a time' }, row('run_4', 'sch_1', 1)] as never
      await m.catchUp(async () => rows)
      expect(history.list()).toHaveLength(1)
      expect(history.get(history.list()[0].id)!.runs.map(r => r.run_id)).toEqual(['run_4'])
    })
  })
})
