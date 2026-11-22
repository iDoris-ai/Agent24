// M4 A4: a conversation's tasks as cards, and "hand it to the assistant".
//
// A card is data, not a message: it is not in the conversation's messages
// and never goes to the model. It shows the run behind a task (design §3.3):
// where it stands, for how long, and what the user can do about it. Where a
// run stands comes from run-status.ts: asked once, then on the daemon's
// events, and again when the window comes back (the tasks are in the
// conversation's file, docs/ui/ASSISTANT-A.md). Answers get no card.

import { withoutToolNames } from './user-facing'
import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react'
import { useT } from '../../i18n'
import { chatMessages } from '../Chat.i18n'
import { createChatSession } from '../chat-knowledge'
import { AlreadyDecided, cancelRun, createRun, decideApproval, listPendingApprovals, type Approval, type Decision } from '../agent/api'
import { grounding } from '../knowledge-grounding'
import { forgetPromotion, getHistoryState, promoteRun, subscribeHistory } from './history'
import { skillForConversation } from './session-skill'
import { namedDocument } from './doc-intent-run'
import { mismatchedEdit } from './patch-anchor'
import { DocumentsOutcome, documentName } from './DocumentsOutcome'
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
/** What the approval asks, in words (D2b: documents tools by name and what
 *  they make; anything else as the daemon's summary, cut short). */
/** A JSON string value as written in `src` after `"key":`, unescaped; cut
 * short where the summary was (the daemon truncates it). */
function stringAfter(src: string, key: string): string | undefined {
  const m = new RegExp(`"${key}":"((?:[^"\\\\]|\\\\.)*)`).exec(src)
  if (!m) return undefined
  m[1] = m[1].replace(/…? \[truncated \d+ bytes\]$/, '…')
  try { return JSON.parse(`"${m[1].replace(/\\$/, '')}"`) as string } catch { return m[1] }
}

/** M1 (0.6.78): the daemon cuts an approval's summary at about 200 bytes
 * (`summarize_input`, 「… [truncated N bytes]」), so a patch with real text never
 * parses and the card showed raw JSON. Its document and edits are read off
 * what is there: each edit's index, and as much of its text as made it. */
export function patchArgsOf(json: string): { document_id: string; edits: Array<Record<string, unknown>> } | null {
  const doc = stringAfter(json, 'document_id')
  if (doc === undefined) return null
  const start = json.indexOf('"edits":[')
  const edits = start < 0
    ? []
    : json.slice(start + 9).split(/\},\s*\{/).flatMap((part) => {
      const index = /"index":(\d+)/.exec(part)
      if (!index) return []
      const edit: Record<string, unknown> = { index: Number(index[1]) }
      for (const key of ['text', 'anchor', 'expect']) {
        const v = stringAfter(part, key)
        if (v !== undefined) edit[key] = v
      }
      if (/"tracked":true/.test(part)) edit['tracked'] = true
      return [edit]
    })
  return { document_id: doc, edits }
}

/** PR-Daemon #1986 A: what the approval asks, from its full `payload` (the
 * tool's whole input, as the daemon stores and lists it), never only the
 * summary the daemon cuts at about 200 bytes: cut, a patch of 3 tracked edits
 * read as 「改写 1 段」, and an `"index":12` cut to `1` was auto-denied. `complete`
 * is false only when the payload is missing and the summary was cut. */
export function approvalArgs(a: Pick<Approval, 'summary'> & { payload?: unknown }): { tool: string; args: Record<string, unknown>; complete: boolean } {
  const summary = typeof a.summary === 'string' ? a.summary : ''
  const cut = summary.indexOf(': ')
  const tool = cut > 0 ? summary.slice(0, cut) : summary
  const payload = a.payload
  // PR-Daemon #1987 Low 2: a patch payload without its edits is no whole input.
  if (payload && typeof payload === 'object' && !Array.isArray(payload) && Object.keys(payload).length > 0
    && (tool !== 'documents.patch' || Array.isArray((payload as Record<string, unknown>)['edits']))) {
    return { tool, args: payload as Record<string, unknown>, complete: true }
  }
  try {
    return { tool, args: cut > 0 ? JSON.parse(summary.slice(cut + 2)) as Record<string, unknown> : {}, complete: true }
  } catch {
    // A summary cut short with its text: read what is there, and say so.
    return { tool, args: tool === 'documents.patch' ? patchArgsOf(summary.slice(cut + 2)) ?? {} : {}, complete: false }
  }
}

/** PR-Daemon #2004 B2: how deep and how long 「详情」 goes before it says the
 *  rest is folded; a payload nested ten thousand deep no longer throws. */
const DETAIL_MAX_DEPTH = 8
const DETAIL_MAX_LINES = 400
/** PR-Daemon #2004 B1: a value never lays out lines of its own: a string is
 *  shown as a JSON string (its line breaks as `\n`, quoted, so 12 and "12"
 *  differ), and characters that reorder text are escaped too. */
const REORDER = /[\u2028\u2029\u202a-\u202e\u2066-\u2069]/g
const escaped = (s: string) => JSON.stringify(s).replace(REORDER, c => `\\u${c.charCodeAt(0).toString(16)}`)
const shownValue = (v: unknown) => (typeof v === 'string' ? escaped(v) : String(v))
const shownKey = (k: string) => (/^[\p{L}\p{N}_.-]+$/u.test(k) ? k : escaped(k))

/** UI polish 09: 「详情」 under the card's line, the step's whole input as a
 *  list, key by key, nested arrays and objects in full, nothing cut. With no
 *  payload (an older daemon) the summary as the daemon wrote it. */
export function approvalDetail(
  a: Pick<Approval, 'summary'> & { payload?: unknown },
  t: (key: string, vars?: Record<string, string>) => string,
): string {
  const summary = typeof a.summary === 'string' ? a.summary : ''
  const payload = a.payload
  if (!payload || typeof payload !== 'object' || Array.isArray(payload) || Object.keys(payload).length === 0) return summary
  const cut = summary.indexOf(': ')
  const tool = cut > 0 ? summary.slice(0, cut) : summary
  // PR-Daemon #2004 Low: a patch without its edits is not whole (as in
  // `approvalArgs`): the summary, not a list that looks complete.
  if (tool === 'documents.patch' && !Array.isArray((payload as Record<string, unknown>)['edits'])) return summary
  const lines: string[] = []
  let folded = false
  const walk = (v: unknown, pad: string, head: string, depth: number) => {
    if (lines.length >= DETAIL_MAX_LINES) { folded = true; return }
    if (v === null || typeof v !== 'object') lines.push(`${head}${shownValue(v)}`)
    else if (Object.keys(v).length === 0) lines.push(`${head}${t('task.detail.empty')}`)
    else if (depth >= DETAIL_MAX_DEPTH) lines.push(`${head}${t('task.detail.folded')}`)
    else {
      lines.push(head.trimEnd())
      const entries = Array.isArray(v) ? v.map((x, i) => [`${i + 1}. `, x] as const) : Object.entries(v as Record<string, unknown>).map(([k, x]) => [`${shownKey(k)}：`, x] as const)
      for (const [label, x] of entries) walk(x, `${pad}  `, `${pad}  ${label}`, depth + 1)
    }
  }
  for (const [k, x] of Object.entries(payload as Record<string, unknown>)) walk(x, '', `${shownKey(k)}：`, 1)
  if (folded) lines.push(t('task.detail.folded'))
  return [t('task.detail.tool', { tool: withoutToolNames(tool) || tool }), ...lines].join('\n')
}

export function approvalText(
  a: Pick<Approval, 'summary'> & { payload?: unknown },
  t: (key: string, vars?: Record<string, string>) => string,
  /** M1 (0.6.75): a patch's paragraphs as the document has them, by index.
   * The card shows these, not the model's `expect`. */
  originals: ReadonlyMap<number, string> | null = null,
  /** General 0.6.94: a document id the card names, by its title. */
  titles: ReadonlyMap<string, string> | null = null,
): string {
  const { tool, args, complete } = approvalArgs(a)
  const text = (v: unknown) => (typeof v === 'string' ? v : '')
  const str = (v: unknown) => titles?.get(text(v)) ?? text(v)
  if (tool === 'documents.list') return t('task.ask.list')
  if (tool === 'documents.get') return t('task.ask.get', { doc: str(args['document_id']) })
  if (tool === 'documents.compose' && Array.isArray(args['parts'])) {
    const parts = (args['parts'] as Array<Record<string, unknown>>)
      // UI polish 08: no pages is the whole document, said so (not 「第 all 页」).
      .map(p => (str(p['pages'])
        ? t('task.ask.part', { doc: str(p['document_id']), pages: str(p['pages']) })
        : t('task.ask.partAll', { doc: str(p['document_id']) })))
      .join(' + ')
    return t('task.ask.compose', { title: str(args['title']), parts })
  }
  if (tool === 'documents.paragraphs') return t('task.ask.paragraphs', { doc: str(args['document_id']) })
  if (tool === 'documents.fields') return t('task.ask.fields', { doc: str(args['document_id']) })
  if (tool === 'documents.fill' && args['values'] && typeof args['values'] === 'object') {
    const values = Object.entries(args['values'] as Record<string, unknown>)
      .map(([k, v]) => `${k}=${str(v)}`)
      .join('，')
    return t('task.ask.fill', { doc: str(args['document_id']), values: values.length > 300 ? `${values.slice(0, 300)}…` : values })
  }
  if (tool === 'documents.patch' && Array.isArray(args['edits'])) {
    const edits = args['edits'] as Array<Record<string, unknown>>
    const head = t(edits.some(e => e['tracked'] === true) ? 'task.ask.patchTracked' : 'task.ask.patch', { doc: str(args['document_id']), n: complete ? String(edits.length) : t('task.ask.atLeast', { n: String(edits.length) }) })
    // M1 (0.6.72): the card said only 「改写 1 段」 while a model was about to
    // write 第三条's text over 乙方's line. Each edit shows what it replaces.
    const clip = (s: string) => (s.length > 60 ? `${s.slice(0, 60)}…` : s)
    const shown = edits.slice(0, 5).map(e => {
      const index = typeof e['index'] === 'number' ? e['index'] : null
      const real = index === null ? undefined : originals?.get(index)
      // M1 (c), PM 11-21: the paragraph as the document has it, never the
      // model's retelling; until it is read, the card says so.
      if (real === undefined) return t('task.ask.patchUnread', { n: index === null ? '?' : String(index + 1), to: clip(text(e['text'])) })
      return t('task.ask.patchEdit', {
        n: index === null ? '?' : String(index + 1),
        from: clip(real),
        to: clip(text(e['text'])),
      })
    })
    return [head, ...shown, ...(edits.length > 5 ? ['…'] : [])].join('\n')
  }
  // M3/PM 11-21: never raw JSON on the card; the summary is under 「详情」.
  return t('task.ask.tool', { tool: withoutToolNames(tool) || tool })
}

/** M1 (0.6.75): the paragraphs a pending patch rewrites, read from the
 * document itself (its id, or the title the model named), so the card's
 * 「原文」 is the paragraph at that index and never only what the model says
 * it is. Null while not read or when it cannot be. */
export async function patchOriginals(api: Window['agent24']['documents'] | undefined, approval: (Pick<Approval, 'summary'> & { payload?: unknown }) | null | undefined): Promise<Map<number, string> | null> {
  if (!api || !approval) return null
  const { tool, args } = approvalArgs(approval)
  if (tool !== 'documents.patch') return null
  const doc = typeof args['document_id'] === 'string' ? args['document_id'] : ''
  if (!doc) return null
  let id = doc
  if (!/^doc_/.test(doc)) {
    const named = await namedDocument(api, doc.replace(/^《|》$/g, '')).catch(() => null)
    if (!named || typeof named === 'string') return null
    id = named.id
  }
  const res = await api.request({ op: 'paragraphs', documentId: id }).catch(() => null)
  if (!res || !res.ok) return null
  return new Map(res.data.paragraphs.map((p) => [p.index, p.text]))
}

/** The tasks of the conversation the page shows. `answered`: runs whose answer the conversation already shows (UI polish
 *  07): their cards say only where they stand, not the answer again. */
export function TaskCards({ conversation, answered }: { conversation: string; answered?: ReadonlySet<string> }) {
  const t = useT(chatMessages)
  const { items } = useSyncExternalStore(subscribeHistory, getHistoryState)
  const tasks = items.find(item => item.id === conversation)?.tasks
  const ids = useMemo(() => (tasks ?? []).map(task => task.run_id), [tasks])
  const views = useRunViews(ids)
  if (!tasks || tasks.length === 0) return null
  return (
    <div className="task-cards" role="list" aria-label={t('task.list')}>
      {tasks.map(task => <TaskCard key={task.run_id} task={task} view={views.get(task.run_id)} answered={answered?.has(task.run_id) === true} />)}
    </div>
  )
}

function TaskCard({ task, view, answered }: { task: ChatHistoryRun; view: RunView | undefined; answered: boolean }) {
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
  // D2b: which approval it waits on, asked again while it waits: a run that
  // goes on to its next tool call waits on a new one without leaving the state.
  const [asking, setAsking] = useState<Approval | null>(null)
  // PR-Daemon #2003 L1: the list could not be read: whether one waits is not
  // known, so the card does not say it goes on.
  const [unread, setUnread] = useState(false)
  useEffect(() => {
    setOffers(null)
    setAsking(null)
    setUnread(false)
    if (!waiting) return
    let live = true
    const look = () => listPendingApprovals()
      .then(list => {
        if (!live) return
        setOffers(offersOf(list, task.run_id))
        setAsking(list.find(a => a.run_id === task.run_id) ?? null)
        setUnread(false)
      })
      .catch(() => { if (live) { setOffers(NO_OFFERS); setUnread(true) } })
    void look()
    const timer = setInterval(() => void look(), 2000)
    return () => { live = false; clearInterval(timer) }
  }, [waiting, task.run_id])
  const [originals, setOriginals] = useState<Map<number, string> | null>(null)
  /** General 0.6.94: the card said 「改写 doc_01M8035X… 的 1 段」; the ids
   * the approval names, by their titles. */
  const [titles, setTitles] = useState<Map<string, string> | null>(null)
  /** A patch's paragraphs are still being read: it cannot be confirmed yet. */
  const [reading, setReading] = useState(false)
  /** M1 (c), PM 11-21: the patch was aimed at the wrong paragraph and was
   * sent back to the model, which reads again; the user is not asked. */
  const [sentBack, setSentBack] = useState<{ index: number; opening: string } | null>(null)
  const askingSummary = asking?.summary
  const askingId = asking?.id
  // Keyed on the approval's id: its payload does not change while it waits.
  const askingRef = useRef(asking)
  askingRef.current = asking
  /** The patch's edits could not all be read (no payload, summary cut): it
   * is not offered for approval (PR-Daemon #1986 A). */
  const askingIncomplete = asking ? !approvalArgs(asking).complete && approvalArgs(asking).tool === 'documents.patch' : false
  // `t` is a new function each render: read through a ref, not a dependency.
  const tRef = useRef(t)
  tRef.current = t
  useEffect(() => {
    setTitles(null)
    const approval = askingRef.current
    if (!approval) return
    const { args } = approvalArgs(approval)
    const parts = Array.isArray(args['parts']) ? args['parts'] as Array<Record<string, unknown>> : []
    const ids = [args['document_id'], ...parts.map(p => p['document_id'])]
      .filter((v): v is string => typeof v === 'string' && /^doc_[0-9A-Z]+$/.test(v))
    if (ids.length === 0) return
    let live = true
    void Promise.all(ids.map(async id => [id, await documentName(window.agent24?.documents, id)] as const))
      .then(named => { if (live) setTitles(new Map(named)) })
    return () => { live = false }
  }, [askingSummary, askingId])
  useEffect(() => {
    setOriginals(null)
    setSentBack(null)
    const isPatch = typeof askingSummary === 'string' && askingSummary.startsWith('documents.patch: ')
    setReading(isPatch)
    if (!isPatch) return
    let live = true
    const approval = askingRef.current
    void patchOriginals(window.agent24?.documents, approval).then(async m => {
      if (!live) return
      setOriginals(m)
      const { args, complete }: { args: Record<string, unknown>; complete: boolean } = approval ? approvalArgs(approval) : { args: {}, complete: false }
      const edits = Array.isArray(args['edits']) ? args['edits'] as Array<Record<string, unknown>> : []
      // Only a whole patch is checked: a cut one is not offered at all.
      const wrong = m && complete ? mismatchedEdit(m, edits) : null
      if (wrong && askingId) {
        setSentBack(wrong)
        const reason = tRef.current('task.anchorSentBack', { n: String(wrong.index + 1), index: String(wrong.index), opening: wrong.opening })
        await decideApproval(askingId, { type: 'deny', reason }).catch(() => {})
        askRunAgain(task.run_id)
      }
      setReading(false)
    }, () => { if (live) setReading(false) })
    return () => { live = false }
  }, [askingSummary, askingId, task.run_id])
  // PR-Daemon #1987 Medium 1: the decision goes to the approval the card
  // shows (`asking`), never to another one of the same run picked again from
  // the list: with two patch approvals in a run, 「确认」 on A approved B. One
  // that is no longer pending says so; nothing else is decided in its place.
  const decide = (decision: Decision) => act(async () => {
    const shown = asking
    if (!shown || !shown.available_decisions.includes(decision.type)) throw new Error(t('task.noApproval'))
    try {
      await decideApproval(shown.id, decision, { alreadyDecided: 'throw' })
    } catch (e) {
      throw e instanceof AlreadyDecided ? new Error(t('task.alreadyDecided')) : e
    }
    setDenying(false)
  })
  const [denying, setDenying] = useState(false)
  const [reason, setReason] = useState('')
  const label = t(labelOf(status))
  // A completed run's answer, with only what its record backs (as replies
  // are, Chat.tsx): the answer may name sources the run never read.
  const grounded = status === 'completed' && view?.output ? grounding(view.output, view.knowledge) : null
  // UI polish 05 (FU-153 on the card): a document instruction is not searched
  // first (as in the bubble, #1997), so 「没有检索知识库」 says nothing there.
  // Unread sources still show.
  const checked = grounded?.kind === 'notSearched' && view?.documents?.attempted === true && grounded.unread.length === 0 ? null : grounded
  return (
    <div className={`task-card${active ? ' active' : ''}${waiting && !sentBack ? ' waiting' : ''}`} role="listitem" aria-label={t('task.card', { status: label })}>
      <span className="task-card-status">{label}</span>
      {status !== 'missing' && !Number.isNaN(from) && <span className="task-card-time">{elapsed(to - from)}</span>}
      {waiting && asking && !sentBack && <span className="task-card-ask">{approvalText(asking, t as (key: string, vars?: Record<string, string>) => string, originals, titles)}</span>}
      {waiting && asking && !sentBack && typeof asking.summary === 'string' && (
        <details className="message-detail"><summary>{t('fail.detail')}</summary>
          <div className="message-detail-list">{approvalDetail(asking, t as (key: string, vars?: Record<string, string>) => string)}</div></details>
      )}
      {sentBack && <span className="task-card-knowledge" role="note">{t('task.anchorNote', { n: String(sentBack.index + 1) })}</span>}
      {status === 'failed' && view?.error && <span className="task-card-error">{view.error}</span>}
      {failed && <span className="task-card-error" role="alert">{failed}</span>}
      {status === 'completed' && view?.output && !answered && (
        <div className="task-card-output">
          {withoutToolNames(view.output)}
          <DocumentsOutcome documents={view.documents} />
          {checked && view.knowledge && <div className="task-card-knowledge">{checked.kind === 'read' ? t('knowledge.via') : t(`knowledge.${checked.kind}`)}</div>}
          {checked && checked.unread.length > 0 && (
            <div className="task-card-knowledge" role="note">{t('knowledge.unread', { ids: checked.unread.join(', ') })}</div>
          )}
        </div>
      )}
      <span className="task-card-actions">
        {waiting && offers?.approve && !denying && !sentBack && !askingIncomplete && (
          <button type="button" className="btn btn-primary task-card-button" disabled={busy || reading} onClick={() => void decide({ type: 'approve' })}>{t('task.confirm')}</button>
        )}
        {waiting && offers?.deny && !denying && (
          <button type="button" className="btn btn-ghost task-card-button" disabled={busy} onClick={() => setDenying(true)}>{t('task.deny')}</button>
        )}
        {waiting && offers !== null && !offers.approve && !offers.deny && (
          // FU-152: no approval of the run pending (just confirmed, the run
          // not yet past the state): it goes on, nothing to decide here.
          <span className="task-card-knowledge" role="note">{t(asking || unread ? 'task.decideInTui' : 'task.proceeding')}</span>
        )}
        {active && (
          // General 0.6.89: 「停止」 never waits on another action of the card
          // (a confirm still open made it grey): stopping is always offered.
          <button type="button" className="btn btn-ghost task-card-button" onClick={() => void act(() => cancelRun(task.run_id))}>{t('task.stop')}</button>
        )}
      </span>
      {waiting && denying && (
        // Loop 3 FU: a reason is welcome, not required; 「拒绝」 with none sends
        // a plain one, rather than a button that silently does nothing.
        <form className="task-card-deny" onSubmit={e => { e.preventDefault(); void decide({ type: 'deny', reason: reason.trim() || t('task.denyDefault') }) }}>
          <input className="task-card-deny-input" aria-label={t('task.denyReason')} placeholder={t('task.denyReason')} value={reason}
            onChange={e => setReason(e.target.value)} disabled={busy} autoFocus />
          <button type="submit" className="btn btn-primary task-card-button" disabled={busy}>{t('task.denySend')}</button>
          <button type="button" className="btn btn-ghost task-card-button" disabled={busy} onClick={() => setDenying(false)}>{t('task.denyCancel')}</button>
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
      const skillId = skillForConversation(conversation)
      const run = await createRun({ prompt, session_id: session, ...(skillId ? { skill_id: skillId } : {}) })
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
