import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createChatHistoryStore, onChatHistoryChange } from './chat-history'
import { createScheduleConversations } from './schedule-conversations'
import { createAssistantApprovals } from './assistant-approvals'

// M4 A6: A5 (a schedule's runs) and A6 (approvals of runs no conversation
// has) on one chat history, wired as main.ts wires them: a schedule's run
// gets one card in all, and none is lost (QA #1690, on #1705 14546b2e).

let dir: string
let history: ReturnType<typeof createChatHistoryStore>
const RUN = 'run_s1'
const daemonRuns = (...ids: string[]) => async () => ids.map((id, i) => ({ id, schedule_id: 'sch_1', created_at: `2026-11-09T0${i}:00:00Z` }))
const pending = (...ids: string[]) => async () => ids.map((run_id, i) => ({ id: `apr_${i}`, run_id }))
/** How many cards the run has, over every conversation. */
const cards = (id = RUN) => history.list().flatMap(c => c.tasks.map(t => t.run_id)).filter(r => r === id).length
const homeOf = (id = RUN) => history.list().find(c => c.tasks.some(t => t.run_id === id))?.title

/** A5 and A6 as main.ts makes them; `a6History` is A6's view of the store. */
function wire(a6History: typeof history = history) {
  const a5 = createScheduleConversations({ dir, history, scheduleName: async () => '周报', title: n => `日程：${n}` })
  const a6 = createAssistantApprovals({
    dir, history: a6History, title: () => '助理发起',
    ownerOf: async () => ((await a5.claim('sch_1', RUN)) ? 'schedule' : 'other'),
    settled: (id, recorded) => a5.settle(id, recorded),
  })
  return { a5, a6 }
}
/** The store, for A6, failing its next append once. */
function a6FailingOnce(): typeof history {
  let fail = true
  return {
    ...history,
    appendRun: (...a: Parameters<typeof history.appendRun>) => {
      if (fail) { fail = false; throw new Error('disk full') }
      return history.appendRun(...a)
    },
  }
}
/** A5's live record of the run, failing once. */
async function a5RecordFails(a5: ReturnType<typeof wire>['a5']): Promise<void> {
  const append = vi.spyOn(history, 'appendRun').mockImplementationOnce(() => { throw new Error('disk full') })
  await expect(a5.record('sch_1', RUN)).rejects.toThrow('disk full')
  append.mockRestore()
}

beforeEach(() => { dir = fs.mkdtempSync(path.join(os.tmpdir(), 'schedule-approvals-')); history = createChatHistoryStore(dir) })
afterEach(() => { onChatHistoryChange(null); vi.restoreAllMocks(); fs.rmSync(dir, { recursive: true, force: true }) })

describe('a schedule\'s run between A5 and A6: one card, none lost (M4 A6, QA #1690)', () => {
  it('S1 recorded live by A5, then waiting for approval: one card, in the schedule\'s conversation', async () => {
    const { a5, a6 } = wire()
    await a5.catchUp(daemonRuns())
    await a5.record('sch_1', RUN)
    await a6.route(RUN)
    await a6.catchUp(pending(RUN))
    await a5.catchUp(daemonRuns(RUN))
    expect(cards()).toBe(1)
    expect(homeOf()).toBe('日程：周报')
  })

  it('S2 from before the first catch-up, waiting for approval: one card, in the assistant\'s conversation', async () => {
    const { a5, a6 } = wire()
    await a5.catchUp(daemonRuns(RUN))
    await a6.catchUp(pending(RUN))
    await a5.catchUp(daemonRuns(RUN))
    await a6.catchUp(pending(RUN))
    expect(cards()).toBe(1)
    expect(homeOf()).toBe('助理发起')
  })

  it('S3 A5 could not record it and A6 took it: one card, and later catch-ups add none', async () => {
    const { a5, a6 } = wire()
    await a5.catchUp(daemonRuns())
    await a5RecordFails(a5)
    await a6.route(RUN)
    await a5.catchUp(daemonRuns(RUN))
    await a6.catchUp(pending(RUN))
    await a5.catchUp(daemonRuns(RUN))
    expect(cards()).toBe(1)
  })

  it.each(['A5', 'A6'])('S4 neither could record it, then both catch up at once, %s\'s first: one card', async (first) => {
    const { a5, a6 } = wire(a6FailingOnce())
    await a5.catchUp(daemonRuns())
    await a5RecordFails(a5)
    await expect(a6.route(RUN)).rejects.toThrow('disk full')
    expect(cards()).toBe(0)
    // Connected again: main starts both catch-ups, neither waiting for the other.
    const a5Up = () => a5.catchUp(daemonRuns(RUN))
    const a6Up = () => a6.catchUp(pending(RUN))
    await (first === 'A5' ? Promise.all([a5Up(), a6Up()]) : Promise.all([a6Up(), a5Up()]))
    expect(cards()).toBe(1)
    await Promise.all([a5Up(), a6Up()])
    expect(cards()).toBe(1)
  })

  it('S5 A6 could not record it, and main starts again before A5 retries: one card', async () => {
    const first = wire(a6FailingOnce())
    await first.a5.catchUp(daemonRuns())
    await a5RecordFails(first.a5)
    await expect(first.a6.route(RUN)).rejects.toThrow('disk full')
    const { a5, a6 } = wire()
    await Promise.all([a5.catchUp(daemonRuns(RUN)), a6.catchUp(pending(RUN))])
    await Promise.all([a5.catchUp(daemonRuns(RUN)), a6.catchUp(pending(RUN))])
    expect(cards()).toBe(1)
  })

  it('S6 claimed before the first catch-up, never heard of live, and A6 could not record it: one card later', async () => {
    const { a5, a6 } = wire(a6FailingOnce())
    await Promise.all([a6.catchUp(pending(RUN)), a5.catchUp(daemonRuns(RUN))])
    await Promise.all([a5.catchUp(daemonRuns(RUN)), a6.catchUp(pending(RUN))])
    expect(cards()).toBe(1)
  })

  it('S7 recorded live and waiting for approval at once: one card', async () => {
    const { a5, a6 } = wire()
    await a5.catchUp(daemonRuns())
    await Promise.all([a5.record('sch_1', RUN), a6.route(RUN)])
    await Promise.all([a5.catchUp(daemonRuns(RUN)), a6.catchUp(pending(RUN))])
    expect(cards()).toBe(1)
  })

  it('S8 neither could record it, and the approval was decided in agent24 tui since: A5 records it, it is not lost (Codex A6 r3 C2)', async () => {
    const { a5, a6 } = wire(a6FailingOnce())
    await a5.catchUp(daemonRuns())
    await a5RecordFails(a5)
    await expect(a6.route(RUN)).rejects.toThrow('disk full')
    await Promise.all([a5.catchUp(daemonRuns(RUN)), a6.catchUp(pending())])
    expect(cards()).toBe(1)
    expect(homeOf()).toBe('日程：周报')
  })

  it('N8 A5 could not record it live, A6 claimed it, the first catch-up came between, and A6 could not record it: A5 records it, one card (#1719 C1)', async () => {
    const a6History = a6FailingOnce()
    const a5 = createScheduleConversations({ dir, history, scheduleName: async () => '周报', title: n => `日程：${n}` })
    const a6 = createAssistantApprovals({
      dir, history: a6History, title: () => '助理发起',
      // The first catch-up lands while A6 holds the claim.
      ownerOf: async () => {
        const mine = await a5.claim('sch_1', RUN)
        await a5.catchUp(daemonRuns(RUN))
        return mine ? 'schedule' : 'other'
      },
      settled: (id, recorded) => a5.settle(id, recorded),
    })
    await a5RecordFails(a5)
    await expect(a6.route(RUN)).rejects.toThrow('disk full')
    expect(cards()).toBe(0)
    await Promise.all([a5.catchUp(daemonRuns(RUN)), a6.catchUp(pending())])
    expect(cards()).toBe(1)
    expect(homeOf()).toBe('日程：周报')
  })

  it('N9 A6\'s claim could not be written: nothing is reserved, A5 records it as before, one card (#1719 C2)', async () => {
    const { a5, a6 } = wire()
    await a5.catchUp(daemonRuns())
    await a5RecordFails(a5)
    const handledBefore = JSON.parse(fs.readFileSync(path.join(dir, 'schedule-conversations.json'), 'utf8')).handled
    // A6 writes nothing before it asks the owner: the next write is the claim's.
    const write = vi.spyOn(fs, 'writeFileSync').mockImplementationOnce(() => { throw new Error('disk full') })
    await expect(a6.route(RUN)).rejects.toThrow('disk full')
    write.mockRestore()
    expect(JSON.parse(fs.readFileSync(path.join(dir, 'schedule-conversations.json'), 'utf8')).handled).toEqual(handledBefore)
    await Promise.all([a5.catchUp(daemonRuns(RUN)), a6.catchUp(pending())])
    expect(cards()).toBe(1)
    expect(homeOf()).toBe('日程：周报')
    await Promise.all([a5.catchUp(daemonRuns(RUN)), a6.catchUp(pending(RUN))])
    expect(cards()).toBe(1)
  })
})
