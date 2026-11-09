// @vitest-environment jsdom
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createChatHistoryStore, onChatHistoryChange } from '../../../main/chat-history'
import { CHAT_HISTORY_FORMAT, CHAT_HISTORY_VERSION, type ChatHistoryRecord, type ChatHistorySummary } from '../../../shared/chat-history'
import { deleteConversation, getHistoryState, promoteRun, resetHistoryStore, saveConversation, setRetrySleepForTests, subscribeHistory } from './history'

// M4 A2: the page's side of a conversation's runs, against main's real store
// behind a bridge that calls it as the IPC handlers and main.ts do.

let dir: string
let store: ReturnType<typeof createChatHistoryStore>
let bridge: Record<string, unknown>
const wrap = <A extends unknown[], R>(f: (...a: A) => R) => vi.fn(async (...a: A) => f(...a))

function record(id: string, prompt: string, at = '2026-11-08T01:00:00.000Z'): ChatHistoryRecord {
  return {
    format: CHAT_HISTORY_FORMAT, version: CHAT_HISTORY_VERSION, id, title: prompt, title_source: 'prompt',
    group_id: null, archived_at: null, created_at: at, updated_at: at, runs: [], pending: [],
    messages: [{ role: 'user', content: prompt, at, delivered: true }],
  }
}
const item = (id: string) => getHistoryState().items.find(i => i.id === id)
const flush = () => new Promise<void>(resolve => setTimeout(resolve, 0))

beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'runs-'))
  store = createChatHistoryStore(dir)
  const changed = new Set<(s: ChatHistorySummary) => void>()
  // main.ts forwards main's own changes to the window.
  onChatHistoryChange(summary => { for (const cb of changed) cb(summary) })
  bridge = {
    list: wrap(() => store.list()), get: wrap((id: string) => store.get(id)), save: wrap((r: unknown) => store.save(r)),
    delete: wrap((id: string) => store.delete(id)), clear: wrap(() => store.clear()),
    promoteRun: wrap((id: string, runId: string) => store.promoteRun(id, runId)),
    onChanged: (cb: (s: ChatHistorySummary) => void) => { changed.add(cb); return () => changed.delete(cb) },
  }
  Object.defineProperty(window, 'agent24', { value: { chatHistory: bridge }, writable: true, configurable: true })
  resetHistoryStore()
})
afterEach(() => { onChatHistoryChange(null); fs.rmSync(dir, { recursive: true, force: true }) })

describe('a conversation\'s runs on the page (M4 A2)', () => {
  it('promoteRun shows the task from what main wrote', async () => {
    await saveConversation(record('chat_a', '周报'))
    expect(item('chat_a')?.tasks).toEqual([])
    expect(await promoteRun('chat_a', 'run_1')).toBe(true)
    expect(item('chat_a')?.tasks).toEqual([expect.objectContaining({ run_id: 'run_1', kind: 'task' })])
  })

  it('promoteRun of a conversation not on file, or deleted meanwhile, shows nothing', async () => {
    expect(await promoteRun('chat_none', 'run_1')).toBe(false)
    await saveConversation(record('chat_a', '周报'))
    const promoted = promoteRun('chat_a', 'run_1')
    await deleteConversation('chat_a')
    expect(await promoted).toBe(false)
    expect(item('chat_a')).toBeUndefined()
  })

  it('promotes a run once the conversation\'s first save lands, when that save was still being retried (Codex A2 C2)', async () => {
    let release!: () => void
    setRetrySleepForTests(() => new Promise<void>(resolve => { release = resolve }))
    const save = bridge.save as ReturnType<typeof vi.fn>
    save.mockRejectedValueOnce(new Error('disk busy'))
    const saved = saveConversation(record('chat_a', '周报'))
    await flush()
    // The run waits on its approval while the first save waits to retry.
    expect(await promoteRun('chat_a', 'run_1')).toBe(false)
    release()
    expect(await saved).toBe(true)
    await flush()
    expect(item('chat_a')?.tasks).toEqual([expect.objectContaining({ run_id: 'run_1', kind: 'task' })])
  })

  it('keeps the promotion when the save\'s last try fails before main answers it (Codex A2 r2)', async () => {
    const releases: (() => void)[] = []
    setRetrySleepForTests(() => new Promise<void>(resolve => { releases.push(resolve) }))
    const save = bridge.save as ReturnType<typeof vi.fn>
    let failLast!: (e: Error) => void
    save.mockRejectedValueOnce(new Error('disk busy')).mockRejectedValueOnce(new Error('disk busy'))
      .mockImplementationOnce(() => new Promise((_, reject) => { failLast = reject }))
    let answer!: (s: null) => void
    ;(bridge.promoteRun as ReturnType<typeof vi.fn>).mockImplementationOnce(() => new Promise(resolve => { answer = resolve }))
    const saved = saveConversation(record('chat_a', '周报'))
    for (let i = 0; i < 2; i++) { await flush(); releases[i]() }
    await flush()
    const promoted = promoteRun('chat_a', 'run_1')
    failLast(new Error('disk busy'))
    expect(await saved).toBe(false)
    answer(null)
    expect(await promoted).toBe(false)
    // The page writes the conversation again on its next change.
    expect(await saveConversation(record('chat_a', '周报', '2026-11-08T02:00:00.000Z'))).toBe(true)
    await flush()
    expect(item('chat_a')?.tasks).toEqual([expect.objectContaining({ run_id: 'run_1', kind: 'task' })])
  })

  it('tries a promotion main could not take again after the next save; one for no conversation is dropped', async () => {
    await saveConversation(record('chat_a', '周报'))
    ;(bridge.promoteRun as ReturnType<typeof vi.fn>).mockRejectedValueOnce(new Error('Chat history unreadable'))
    expect(await promoteRun('chat_a', 'run_1')).toBe(false)
    expect(await promoteRun('chat_b', 'run_2')).toBe(false)
    await saveConversation(record('chat_b', '日报'))
    expect(await saveConversation(record('chat_a', '周报', '2026-11-08T02:00:00.000Z'))).toBe(true)
    await flush()
    expect(item('chat_a')?.tasks).toEqual([expect.objectContaining({ run_id: 'run_1' })])
    expect(item('chat_b')?.tasks).toEqual([])
  })

  it('reads the list again when main\'s own modules change a conversation', async () => {
    const off = subscribeHistory(() => {})
    await saveConversation(record('chat_a', '周报'))
    // A schedule's run, recorded by a main module (A5), not by the page.
    store.appendRun('chat_a', 'run_9', 'schedule')
    await flush()
    expect(item('chat_a')?.tasks).toEqual([expect.objectContaining({ run_id: 'run_9', kind: 'schedule' })])
    off()
  })
})
