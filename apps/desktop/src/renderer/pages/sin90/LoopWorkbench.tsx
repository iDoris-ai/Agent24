import { useEffect, useState } from 'react'
import { Button, Select, TextArea } from '../../ui'
import { useT } from '../../i18n'
import { fetchCompletionMaterial, fetchPriority, fetchTasks } from './api'
import { isoWeekUTC, type Sin90Task, type Sin90Today } from './model'
import { isQuadrant, quadrants, type CompletionEvidence, type Priority } from './loop-model'
import { sin90Messages } from './Sin90.i18n'

type Write = (body: Record<string, unknown>) => void

export function PriorityBoard({ today, enabled, write, complete }: {
  today: Sin90Today; enabled: boolean; write: Write; complete: (task: Sin90Task) => void
}) {
  const t = useT(sin90Messages)
  const [tasks, setTasks] = useState<Sin90Task[]>([])
  const [priorities, setPriorities] = useState<Map<string, Priority | null>>(new Map())
  const [loading, setLoading] = useState(true)
  const [advice, setAdvice] = useState('')
  const [panelOpen, setPanelOpen] = useState(false)
  useEffect(() => {
    let live = true
    setLoading(true)
    void (async () => {
      const all = await fetchTasks().catch(() => [])
      const rows = [...new Map([...all, ...today.must_do, ...today.inbox, ...today.carry_over_candidates].map((task) => [task.id, task])).values()]
        .filter((task) => !['done', 'dropped', 'carried_over'].includes(task.status))
      const records = await Promise.all(rows.map(async (task): Promise<[string, Priority | null]> => [task.id, await fetchPriority(task.id).catch(() => null)]))
      if (live) { setTasks(rows); setPriorities(new Map(records)); setLoading(false) }
    })()
    return () => { live = false }
  }, [today])
  const supported = [...priorities.values()].some((priority) => priority !== null)
  const canEvaluate = enabled && !loading && supported
  const submitEvaluation = () => {
    const background = advice.trim()
    write({ op: 'evaluatePriority', ...(background ? { advice: background } : {}) })
  }
  return <section aria-label={t('priorityBoard')} className="sin90-stack">
    <div className="sin90-row"><h2>{t('priorityBoard')}</h2><Button aria-expanded={panelOpen} disabled={!canEvaluate} onClick={() => setPanelOpen(!panelOpen)}>{t('evaluatePriority')}</Button></div>
    {panelOpen && <div className="sin90-priority-panel" role="region" aria-label={t('evaluatePriority')}>
      <p className="sin90-muted">{t('priorityPanelNote')}</p>
      <TextArea id="priority-advice" label={t('priorityAdvice')} value={advice} disabled={!canEvaluate} onChange={(event) => setAdvice(event.target.value)} />
      <div className="sin90-actions"><Button variant="primary" disabled={!canEvaluate} onClick={submitEvaluation}>{t('startEvaluation')}</Button><Button disabled={!enabled} onClick={() => { setPanelOpen(false); setAdvice('') }}>{t('cancel')}</Button></div>
    </div>}
    {loading ? <p role="status">{t('loading')}</p> : <>
      {!supported && <p className="sin90-muted">{t('priorityUnavailable')}</p>}
      <div className="sin90-quadrants">{[...quadrants, 'unevaluated' as const].map((quadrant) => {
        const rows = tasks.filter((task) => (priorities.get(task.id)?.effective?.quadrant ?? 'unevaluated') === quadrant)
        const title = quadrant === 'unevaluated' ? t('unevaluated') : t(`q.${quadrant}`)
        return <section aria-label={title} className={`sin90-quadrant sin90-quadrant-${quadrant}`} key={quadrant}>
          <h3>{title}</h3>{quadrant !== 'unevaluated' && <p>{t(`qAction.${quadrant}`)}</p>}
          <ul className="sin90-queue">{rows.map((task) => <li key={task.id} aria-label={task.title} className="sin90-quadrant-task">
            <div className="sin90-row"><Button size="sm" disabled={!enabled} onClick={() => complete(task)}>{t('action.done')}</Button><strong>{task.title}</strong></div>
            <PriorityEditor task={task} priority={priorities.get(task.id) ?? null} enabled={enabled} write={write} />
          </li>)}</ul>{rows.length === 0 && <p className="sin90-muted">{t('none')}</p>}
        </section>
      })}</div>
    </>}
  </section>
}
function PriorityEditor({ task, priority, enabled, write }: { task: Sin90Task; priority: Priority | null; enabled: boolean; write: Write }) {
  const t = useT(sin90Messages)
  const [quadrant, setQuadrant] = useState(priority?.effective?.quadrant ?? 'schedule')
  const [reason, setReason] = useState(priority?.human_override?.rationale ?? '')
  return <details><summary>{t('priorityDetails')}</summary>
    <p>{t('aiAssessment')}：{priority?.ai ? `${t(`q.${priority.ai.quadrant}`)} · ${priority.ai.rationale}` : t('noAssessment')}</p>
    {priority?.human_override && <p>{t('myDecision')}：{t(`q.${priority.human_override.quadrant}`)} · {priority.human_override.rationale}</p>}
    <Select id={`priority-${task.id}`} label={t('myDecision')} value={quadrant} disabled={!enabled || !priority} onChange={(event) => { if (isQuadrant(event.target.value)) setQuadrant(event.target.value) }}>{quadrants.map((value) => <option key={value} value={value}>{t(`q.${value}`)}</option>)}</Select>
    <TextArea id={`priority-reason-${task.id}`} label={t('priorityReason')} value={reason} maxLength={2000} disabled={!enabled || !priority} onChange={(event) => setReason(event.target.value)} />
    <div className="sin90-actions"><Button disabled={!enabled || !priority} onClick={() => write({ op: 'overridePriority', id: task.id, quadrant, rationale: reason })}>{t('saveDecision')}</Button><Button disabled={!enabled || !priority?.human_override} onClick={() => write({ op: 'clearPriorityOverride', id: task.id })}>{t('clearDecision')}</Button></div>
  </details>
}

export function CompletionMaterial({ revision, enabled, write }: { revision: Sin90Today | null; enabled: boolean; write: Write }) {
  const t = useT(sin90Messages)
  const [rows, setRows] = useState<CompletionEvidence[] | null>(null)
  useEffect(() => {
    let live = true
    void fetchCompletionMaterial(isoWeekUTC(new Date())).catch(() => null).then((value) => { if (live) setRows(value) })
    return () => { live = false }
  }, [revision])
  return <section aria-label={t('completionMaterial')} className="sin90-stack"><h2>{t('completionMaterial')}</h2>
    <p className="sin90-muted">{t('reviewProposalOnly')}</p>
    {rows === null ? <p>{t('completionUnavailable')}</p> : rows.length === 0 ? <p>{t('noCompletions')}</p> : rows.map((row) => <CompletionRow key={`${row.task_id}-${row.note}`} row={row} enabled={enabled} write={write} />)}
  </section>
}
function CompletionRow({ row, enabled, write }: { row: CompletionEvidence; enabled: boolean; write: Write }) {
  const t = useT(sin90Messages)
  const [note, setNote] = useState(row.note ?? '')
  return <article aria-label={row.title} className="sin90-proposal"><strong>{row.title}</strong><small>{t('completedAt', { time: row.completed_at })}</small>
    <p>{t('completionQuadrant')}：{row.quadrant ? t(`q.${row.quadrant}`) : t('unevaluated')}</p>
    <p>{row.note || t('noCompletionNote')}</p>
    <TextArea id={`completion-${row.task_id}`} label={t('completionNote')} value={note} maxLength={4000} disabled={!enabled} onChange={(event) => setNote(event.target.value)} />
    <Button disabled={!enabled} onClick={() => write({ op: 'completionNote', id: row.task_id, note })}>{t('saveCompletionNote')}</Button>
  </article>
}
