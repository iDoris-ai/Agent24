import { useEffect, useState } from 'react'
import { Button, Select, TextArea } from '../../ui'
import { useLocale, useT } from '../../i18n'
import { fetchCompletionMaterial, fetchPriority, fetchTasks } from './api'
import { isoWeekUTC, type Sin90Task, type Sin90Today } from './model'
import { isQuadrant, quadrants, type CompletionEvidence, type Priority, type Quadrant } from './loop-model'
import { sin90Messages } from './Sin90.i18n'

type Write = (body: Record<string, unknown>) => void

function useWeeklyCompletions(revision: Sin90Today | null) {
  const [rows, setRows] = useState<CompletionEvidence[] | null>(null)
  useEffect(() => {
    let live = true
    void fetchCompletionMaterial(isoWeekUTC(new Date())).catch(() => null).then((value) => { if (live) setRows(value) })
    return () => { live = false }
  }, [revision])
  return rows
}

export function PriorityBoard({ today, enabled, write, complete }: {
  today: Sin90Today; enabled: boolean; write: Write; complete: (task: Sin90Task) => void
}) {
  const t = useT(sin90Messages)
  const [tasks, setTasks] = useState<Sin90Task[]>([])
  const [priorities, setPriorities] = useState<Map<string, Priority | null>>(new Map())
  const [loading, setLoading] = useState(true)
  const completions = useWeeklyCompletions(today)
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
  const quadrantOf = (task: Sin90Task): Quadrant | null => priorities.get(task.id)?.effective?.quadrant ?? null
  return <section aria-label={t('priorityBoard')} className="sin90-stack">
    <h2>{t('priorityBoard')}</h2>
    {loading ? <p role="status">{t('loading')}</p> : <>
      {tasks.length > 0 && !supported && <p className="sin90-muted">{t('priorityUnavailable')}</p>}
      <div className="sin90-quadrants">{quadrants.map((quadrant) => {
        const rows = tasks.filter((task) => quadrantOf(task) === quadrant)
        const title = t(`q.${quadrant}`)
        return <section aria-label={title} className={`sin90-quadrant sin90-quadrant-${quadrant}`} key={quadrant}>
          <h3>{title}</h3><p>{t(`qAction.${quadrant}`)}</p>
          <ul className="sin90-queue">{rows.map((task) => {
            const priority = priorities.get(task.id) ?? null
            return <li key={task.id} aria-label={task.title} className="sin90-quadrant-task">
              <div className="sin90-classified-row">
                <strong>{task.title}</strong>
                <span className="sin90-priority-source">{priority?.human_override ? t('priorityHuman') : t('prioritySystem')}</span>
                <Button size="sm" disabled={!enabled} onClick={() => complete(task)}>{t('action.done')}</Button>
              </div>
            </li>
          })}</ul>{rows.length === 0 && <p className="sin90-muted">{t('none')}</p>}
        </section>
      })}</div>
    </>}
    <div className="sin90-priority-split">
      <section aria-label={t('unevaluated')} className="sin90-priority-column">
        <h3>{t('unevaluated')}</h3>
        {loading ? <p role="status">{t('loading')}</p> : <>
          <ul className="sin90-queue">{tasks.filter((task) => quadrantOf(task) === null).map((task) => <UnevaluatedTask key={task.id} task={task} priority={priorities.get(task.id) ?? null} enabled={enabled} write={write} complete={complete} />)}</ul>
          {tasks.every((task) => quadrantOf(task) !== null) && <p className="sin90-muted">{t('none')}</p>}
        </>}
      </section>
      <CompletedWeek rows={completions} />
    </div>
  </section>
}

function UnevaluatedTask({ task, priority, enabled, write, complete }: {
  task: Sin90Task; priority: Priority | null; enabled: boolean; write: Write; complete: (task: Sin90Task) => void
}) {
  const t = useT(sin90Messages)
  return <li aria-label={task.title} className="sin90-quadrant-task">
    <div className="sin90-row"><strong>{task.title}</strong><Button size="sm" disabled={!enabled} onClick={() => complete(task)}>{t('action.done')}</Button></div>
    <Select id={`priority-${task.id}`} className="sin90-unevaluated-pick" label={t('chooseQuadrant')} value="" disabled={!enabled || !priority} onChange={(event) => {
      const quadrant = event.target.value
      if (!isQuadrant(quadrant)) return
      write({ op: 'overridePriority', id: task.id, quadrant, rationale: '' })
    }}>
      <option value="">{t('chooseQuadrantPrompt')}</option>
      {quadrants.map((value) => <option key={value} value={value}>{t(`q.${value}`)}</option>)}
    </Select>
  </li>
}

function CompletedWeek({ rows }: { rows: CompletionEvidence[] | null }) {
  const t = useT(sin90Messages)
  return <section aria-label={t('completedThisWeek')} className="sin90-priority-column sin90-completed">
    <header className="sin90-completed-heading"><h3>{t('completedThisWeek')}</h3><small>{t('weekOnly')}</small></header>
    {rows === null ? <p>{t('completionUnavailable')}</p> : rows.length === 0 ? <p className="sin90-muted">{t('noCompletions')}</p> : <ul className="sin90-completed-scroll">{rows.slice().reverse().map((row) => <CompletedRecord key={`${row.task_id}-${row.completed_at}`} row={row} />)}</ul>}
  </section>
}

function CompletedRecord({ row }: { row: CompletionEvidence }) {
  const t = useT(sin90Messages)
  const [open, setOpen] = useState(false)
  const locale = useLocale()
  const when = new Date(row.completed_at)
  const shortTime = when.toLocaleString(locale, { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' })
  return <li className="sin90-completed-item">
    <button type="button" className="sin90-completed-toggle" aria-expanded={open} title={row.title} onClick={() => setOpen((value) => !value)}>
      <span className="sin90-completed-title">{row.title}</span>
      <time dateTime={row.completed_at}>{shortTime}</time>
    </button>
    {open && <div className="sin90-completed-detail">
      <p>{t('completionQuadrant')}：{row.quadrant ? t(`q.${row.quadrant}`) : t('unevaluated')}</p>
      <strong>{row.title}</strong>
      {row.note && <p>{row.note}</p>}
      <p>{t('completedAt', { time: when.toLocaleString(locale) })}</p>
    </div>}
  </li>
}

export function CompletionMaterial({ revision, enabled, write }: { revision: Sin90Today | null; enabled: boolean; write: Write }) {
  const t = useT(sin90Messages)
  const rows = useWeeklyCompletions(revision)
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
