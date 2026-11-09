// M4 A4: a conversation's tasks as cards, and "hand it to the assistant".
//
// A card is data, not a message: it is not in the conversation's messages
// and never goes to the model. It shows the run behind a task (design §3.3):
// where it stands, for how long, and what the user can do about it. Where a
// run stands comes from run-status.ts: asked once, then on the daemon's
// events, and again when the window comes back (the tasks are in the
// conversation's file, docs/ui/ASSISTANT-A.md). Answers get no card.

import { useEffect, useMemo, useState, useSyncExternalStore } from 'react'
import { useT } from '../../i18n'
import { chatMessages } from '../Chat.i18n'
import { createChatSession } from '../chat-knowledge'
import { cancelRun, createRun, decideApproval, listPendingApprovals, type Approval, type Decision } from '../agent/api'
import { grounding } from '../knowledge-grounding'
import { forgetPromotion, getHistoryState, promoteRun, subscribeHistory } from './history'
import { askRunAgain, useRunViews, type RunView } from './run-status'
import type { ChatHistoryRun } from '../../../shared/chat-history'

const ACTIVE = new Set(['queued', 'running', 'awaiting_approval'])
const LABELS = {
  queued: 'task.status.queued', running: 'task.status.running', awaiting_approval: 'task.status.awaiting',
  completed: 'task.status.completed', failed: 'task.status.failed', cancelled: 'task.status.cancelled',
  missing: 'task.status.missing',
} as const
const labelOf = (status: string | undefined) =>
  (status !== undefined && status in LABELS ? LABELS[status as keyof typeof LABELS] : 'task.status.unknown')

/** Minutes and seconds, or hours too. */
export function elapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000))
  const two = (n: number) => String(n).padStart(2, '0')
  const h = Math.floor(s / 3600)
  return h > 0 ? `${h}:${two(Math.floor((s % 3600) / 60))}:${two(s % 60)}` : `${two(Math.floor(s / 60))}:${two(s % 60)}`
}

// The reason's line, as on the approvals page it replaces (inline, like there).
const DENY_FORM = { display: 'flex', gap: 6, alignItems: 'center', flexBasis: '100%' } as const
const DENY_INPUT = {
  flex: 1, fontSize: 12, padding: '4px 8px', background: 'var(--surface2)', color: 'var(--text)',
  border: '1px solid var(--border)', borderRadius: 4,
} as const

/** What the card can decide of the run's pending approval. */
interface Offers { approve: boolean; deny: boolean }
const NO_OFFERS: Offers = { approve: false, deny: false }
function offersOf(list: readonly Approval[], runId: string): Offers {
  const approval = list.find(a => a.run_id === runId)
  return approval
    ? { approve: approval.available_decisions.includes('approve'), deny: approval.available_decisions.includes('deny') }
    : NO_OFFERS
}
/** The run's pending approval, when it offers this decision. */
function pendingOf(list: readonly Approval[], runId: string, type: 'approve' | 'deny'): Approval | null {
  return list.find(a => a.run_id === runId && a.available_decisions.includes(type)) ?? null
}

/** The tasks of the conversation the page shows. */
export function TaskCards({ conversation }: { conversation: string }) {
  const t = useT(chatMessages)
  const { items } = useSyncExternalStore(subscribeHistory, getHistoryState)
  const tasks = items.find(item => item.id === conversation)?.tasks
  const ids = useMemo(() => (tasks ?? []).map(task => task.run_id), [tasks])
  const views = useRunViews(ids)
  if (!tasks || tasks.length === 0) return null
  return (
    <div className="task-cards" role="list" aria-label={t('task.list')}>
      {tasks.map(task => <TaskCard key={task.run_id} task={task} view={views.get(task.run_id)} />)}
    </div>
  )
}

function TaskCard({ task, view }: { task: ChatHistoryRun; view: RunView | undefined }) {
  const t = useT(chatMessages)
  const status = view?.status
  const active = status !== undefined && ACTIVE.has(status)
  // The time shown goes on while the run does; the daemon is not asked for it.
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!active) return
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [active])
  const from = Date.parse(view?.started_at ?? task.started_at)
  const to = view?.ended_at ? Date.parse(view.ended_at) : now
  const [busy, setBusy] = useState(false)
  const [failed, setFailed] = useState<string | null>(null)
  const act = async (what: () => Promise<unknown>) => {
    setBusy(true)
    setFailed(null)
    try { await what() }
    catch (e) { setFailed(e instanceof Error ? e.message : String(e)) }
    finally {
      setBusy(false)
      askRunAgain(task.run_id)
    }
  }
  // "Confirm" is `approve` and nothing else: this one request. "Deny" is
  // `deny`, with the reason the user gives here (the model is told it, and
  // the run goes on; SPEC-002). The daemon's decisions are an open set
  // (openapi `Decision`); the others grant more (`approve_for_session`,
  // `approve_for_target`) or end the run (`abort`) and are not offered here
  // (Knowledge's review of #1700). What the run's approval offers is asked
  // once it waits, and again when it is decided.
  const waiting = status === 'awaiting_approval'
  const [offers, setOffers] = useState<Offers | null>(null)
  useEffect(() => {
    setOffers(null)
    if (!waiting) return
    let live = true
    listPendingApprovals()
      .then(list => { if (live) setOffers(offersOf(list, task.run_id)) })
      .catch(() => { if (live) setOffers(NO_OFFERS) })
    return () => { live = false }
  }, [waiting, task.run_id])
  const decide = (decision: Decision) => act(async () => {
    const approval = pendingOf(await listPendingApprovals(), task.run_id, decision.type)
    if (!approval) throw new Error(t('task.noApproval'))
    await decideApproval(approval.id, decision)
    setDenying(false)
  })
  const [denying, setDenying] = useState(false)
  const [reason, setReason] = useState('')
  const label = t(labelOf(status))
  // A completed run's answer, with only what its record backs (as replies
  // are, Chat.tsx): the answer may name sources the run never read.
  const checked = status === 'completed' && view?.output ? grounding(view.output, view.knowledge) : null
  return (
    <div className={`task-card${active ? ' active' : ''}`} role="listitem" aria-label={t('task.card', { status: label })}>
      <span className="task-card-status">{label}</span>
      {status !== 'missing' && !Number.isNaN(from) && <span className="task-card-time">{elapsed(to - from)}</span>}
      {status === 'failed' && view?.error && <span className="task-card-error">{view.error}</span>}
      {failed && <span className="task-card-error" role="alert">{failed}</span>}
      {status === 'completed' && view?.output && (
        <div className="task-card-output">
          {view.output}
          {checked && view.knowledge && <div className="task-card-knowledge">{checked.kind === 'read' ? t('knowledge.via') : t(`knowledge.${checked.kind}`)}</div>}
          {checked && checked.unread.length > 0 && (
            <div className="task-card-knowledge" role="note">{t('knowledge.unread', { ids: checked.unread.join(', ') })}</div>
          )}
        </div>
      )}
      <span className="task-card-actions">
        {waiting && offers?.approve && !denying && (
          <button type="button" className="btn" disabled={busy} onClick={() => void decide({ type: 'approve' })}>{t('task.confirm')}</button>
        )}
        {waiting && offers?.deny && !denying && (
          <button type="button" className="btn" disabled={busy} onClick={() => setDenying(true)}>{t('task.deny')}</button>
        )}
        {waiting && offers !== null && !offers.approve && !offers.deny && (
          <span className="task-card-knowledge" role="note">{t('task.decideOnApprovals')}</span>
        )}
        {active && (
          <button type="button" className="btn" disabled={busy} onClick={() => void act(() => cancelRun(task.run_id))}>{t('task.stop')}</button>
        )}
      </span>
      {waiting && denying && (
        <form style={DENY_FORM} onSubmit={e => { e.preventDefault(); if (reason.trim()) void decide({ type: 'deny', reason: reason.trim() }) }}>
          <input style={DENY_INPUT} aria-label={t('task.denyReason')} placeholder={t('task.denyReason')} value={reason}
            onChange={e => setReason(e.target.value)} disabled={busy} autoFocus />
          <button type="submit" className="btn" disabled={busy || reason.trim() === ''}>{t('task.denySend')}</button>
          <button type="button" className="btn" disabled={busy} onClick={() => setDenying(false)}>{t('task.denyCancel')}</button>
        </form>
      )}
    </div>
  )
}

/** Hand-overs on their way, and those that failed, by conversation and
 *  prompt: kept outside the button, so one shown again (after the next turn,
 *  or back from another conversation) does not start the same prompt twice
 *  while the first is still on its way (Codex A4 C2). */
const handing = new Map<string, 'busy' | 'failed'>()
const handingListeners = new Set<() => void>()
function setHanding(key: string, state: 'busy' | 'failed' | null): void {
  if (state === null) handing.delete(key)
  else handing.set(key, state)
  for (const l of handingListeners) l()
}
const subscribeHanding = (l: () => void) => { handingListeners.add(l); return () => { handingListeners.delete(l) } }

/** For tests. */
export function resetHandOversForTests(): void {
  handing.clear()
}

/** "Hand it to the assistant": the prompt as a run of its own, in a new
 *  session, recorded as a task of the conversation it was asked in. */
export function HandOver({ conversation, prompt }: { conversation: string; prompt: string }) {
  const t = useT(chatMessages)
  const key = `${conversation}\n${prompt}`
  const state = useSyncExternalStore(subscribeHanding, () => handing.get(key) ?? 'idle')
  const handOver = async () => {
    if (handing.get(key) === 'busy') return
    setHanding(key, 'busy')
    try {
      const session = await createChatSession(window.agent24.backendProxy)
      if (session === null) throw new Error('session_unavailable')
      const run = await createRun({ prompt, session_id: session })
      // Recorded once main has it; the card shows from then on. One that
      // could not be recorded is not left running unseen.
      if (!(await promoteRun(conversation, run.id))) {
        forgetPromotion(conversation, run.id)
        await cancelRun(run.id).catch(() => {})
        throw new Error('not_recorded')
      }
      setHanding(key, null)
    } catch {
      setHanding(key, 'failed')
    }
  }
  return (
    <span className="task-card-handover">
      <button type="button" className="btn task-card-handover-button" disabled={state === 'busy'} onClick={() => void handOver()}>
        {t('task.handOver')}</button>
      {state === 'failed' && <span role="alert">{t('task.handOverFailed')}</span>}
    </span>
  )
}
