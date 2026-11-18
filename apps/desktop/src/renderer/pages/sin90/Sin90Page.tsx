import { useCallback, useEffect, useRef, useState } from 'react'
import { useT, type T } from '../../i18n'
import { fetchAreas, fetchBlocks, fetchDirections, fetchProposals, fetchReviews, fetchRhythms, fetchRoutines, fetchTasks, fetchToday, fetchWeekAttention, fetchWeeks, sin90Write, writesAvailable } from './api'
import {
  blockTransitions, blocksForDirection, canCarry, currentWeek, firstDirectionId, groupDirections, isoWeekUTC, latestRhythm, nextIsoWeek, routineNext, routinesForDirection, taskTransitions, weekNext, weeklyDraftReviewId,
  type DirectionGroup, type Sin90Block, type Sin90Direction, type Sin90Proposal, type Sin90Review, type Sin90Rhythm, type Sin90Routine, type Sin90Task, type Sin90Today, type Sin90Week, type Sin90WeekAttention,
} from './model'
import { sin90ShouldRefresh } from './refresh'
import { describeOp, sourceLabel } from './sentences'
import { sin90Messages } from './Sin90.i18n'
import './sin90.css'

type Key = keyof typeof sin90Messages.en

function directionLabel(t: T<Key>, status: string): string {
  switch (status) {
    case 'draft': return t('direction.draft')
    case 'active': return t('direction.active')
    case 'paused': return t('direction.paused')
    case 'achieved': return t('direction.achieved')
    case 'abandoned': return t('direction.abandoned')
    default: return t('status.other', { status })
  }
}

function taskLabel(t: T<Key>, status: string): string {
  switch (status) {
    case 'backlog': return t('task.backlog')
    case 'planned': return t('task.planned')
    case 'in_progress': return t('task.in_progress')
    case 'done': return t('task.done')
    case 'dropped': return t('task.dropped')
    case 'carried_over': return t('task.carried_over')
    default: return t('status.other', { status })
  }
}

function TaskActions({ task, enabled, onWrite }: {
  task: Sin90Task
  enabled: boolean
  onWrite: (body: Record<string, unknown>) => void
}) {
  const t = useT(sin90Messages)
  const edges = taskTransitions(task.status)
  return <span className="sin90-actions">
    {edges.map((to) => <button key={to} type="button" className="btn btn-ghost" disabled={!enabled}
      onClick={() => onWrite({ op: 'transitionTask', id: task.id, to })}>{t(`action.${to}`)}</button>)}
    {canCarry(task.status) && <button type="button" className="btn btn-ghost" disabled={!enabled}
      onClick={() => onWrite({ op: 'carryOver', taskId: task.id, isoWeek: nextIsoWeek(new Date()) })}>{t('carry')}</button>}
  </span>
}

function blockTitle(block: Sin90Block, tasks: readonly Sin90Task[]): string | null {
  if (!block.task_id) return null
  return tasks.find((task) => task.id === block.task_id)?.title ?? null
}

function weekLabel(t: T<Key>, status: string): string {
  switch (status) {
    case 'planning': return t('week.planning')
    case 'active': return t('week.active')
    case 'reviewing': return t('week.reviewing')
    case 'closed': return t('week.closed')
    default: return t('status.other', { status })
  }
}

function blockLabel(t: T<Key>, status: string): string {
  switch (status) {
    case 'planned': return t('block.planned')
    case 'started': return t('block.started')
    case 'completed': return t('block.completed')
    case 'skipped': return t('block.skipped')
    default: return t('status.other', { status })
  }
}

function weekAction(t: T<Key>, to: 'active' | 'reviewing' | 'closed'): string {
  if (to === 'active') return t('startWeek')
  if (to === 'reviewing') return t('reviewWeek')
  return t('closeWeek')
}

function Rhythm({ directionId, week, attention, blocks, routines, rhythm, directions, tasks, enabled, minutes, onMinutes, onWrite }: {
  directionId: string
  week: Sin90Week | null
  attention: Sin90WeekAttention | null
  blocks: Sin90Block[]
  routines: Sin90Routine[]
  rhythm: Sin90Rhythm | null
  directions: readonly Sin90Direction[]
  tasks: readonly Sin90Task[]
  enabled: boolean
  minutes: string
  onMinutes: (value: string) => void
  onWrite: (body: Record<string, unknown>) => void
}) {
  const t = useT(sin90Messages)
  const parsed = Number(minutes)
  const minutesOk = Number.isInteger(parsed) && parsed >= 1 && parsed <= 10080
  const step = week ? weekNext(week.status) : null
  const quota = rhythm?.allocations.map((row) => {
    const name = directions.find((direction) => direction.id === row.direction_id)?.title ?? row.direction_id
    return `${name} ${row.pct}%`
  }).join(' · ')
  return <div className="sin90-rhythm" aria-label={t('rhythm')}>
    <h3>{t('rhythm')}</h3>
    {!week && <div className="sin90-row">
      <p>{t('noWeek')}</p>
      <button type="button" className="btn btn-primary" disabled={!enabled}
        onClick={() => onWrite({ op: 'createWeek', isoWeek: isoWeekUTC(new Date()) })}>{t('openWeek')}</button>
    </div>}
    {week && <>
      <div className="sin90-head">
        <p>{t('thisWeek', { week: week.iso_week })}</p>
        <span className="tag">{weekLabel(t, week.status)}</span>
      </div>
      {step && <button type="button" className="btn btn-primary" disabled={!enabled}
        onClick={() => onWrite({ op: 'transitionWeek', id: week.id, to: step })}>{weekAction(t, step)}</button>}
      {attention && <p className="sin90-muted" data-sin90-attention="">{t('attention', {
        planned_min: attention.planned_min, actual_min: attention.actual_min, deviation_min: attention.deviation_min,
      })}</p>}
      <h3>{t('blocks')}</h3>
      {blocks.length === 0 ? <p className="sin90-muted">{t('none')}</p> : <ul className="sin90-queue" aria-label={t('blocks')}>
        {blocks.map((block) => {
          const title = blockTitle(block, tasks)
          return <li key={block.id} className="sin90-row" aria-label={title ?? String(block.planned_minutes)}>
            <span>{t('minutes', { n: block.planned_minutes })}{title ? ` · ${title}` : ''}</span>
            <span className="tag">{blockLabel(t, block.status)}</span>
            <span className="sin90-actions">
              {blockTransitions(block.status).map((to) => <button key={to} type="button" className="btn btn-ghost" disabled={!enabled}
                onClick={() => onWrite({ op: 'transitionBlock', id: block.id, to })}>{t(`action.${to}`)}</button>)}
            </span>
          </li>
        })}
      </ul>}
      <div className="sin90-row">
        <input aria-label={t('blockMinutes')} inputMode="numeric" value={minutes} onChange={(e) => onMinutes(e.target.value)} />
        <button type="button" className="btn btn-primary" disabled={!enabled || !minutesOk}
          onClick={() => onWrite({ op: 'createBlock', plannedMinutes: parsed, directionId })}>{t('addBlock')}</button>
      </div>
    </>}
    {routines.length > 0 && <>
      <h3>{t('routines')}</h3>
      <ul className="sin90-queue">
        {routines.map((routine) => {
          const to = routineNext(routine.status)
          return <li key={routine.id} className="sin90-row" aria-label={routine.title}>
            <span>{routine.title}</span>
            <span className="tag">{routine.status === 'paused' ? t('routine.paused') : t('routine.active')}</span>
            {to && <button type="button" className="btn btn-ghost" disabled={!enabled}
              onClick={() => onWrite({ op: 'transitionRoutine', id: routine.id, to })}>{to === 'paused' ? t('pause') : t('resume')}</button>}
          </li>
        })}
      </ul>
    </>}
    {quota && <p className="sin90-muted">{t('quota')} {quota}</p>}
  </div>
}

export function Sin90Page() {
  const t = useT(sin90Messages)
  const [today, setToday] = useState<Sin90Today | null>(null)
  const [groups, setGroups] = useState<DirectionGroup[] | null>(null)
  const [directions, setDirections] = useState<Sin90Direction[]>([])
  const [selected, setSelected] = useState<string | null>(null)
  const [tasks, setTasks] = useState<Sin90Task[]>([])
  const [weeks, setWeeks] = useState<Sin90Week[]>([])
  const [blocks, setBlocks] = useState<Sin90Block[]>([])
  const [routines, setRoutines] = useState<Sin90Routine[]>([])
  const [rhythms, setRhythms] = useState<Sin90Rhythm[]>([])
  const [proposals, setProposals] = useState<Sin90Proposal[]>([])
  const [reviews, setReviews] = useState<Sin90Review[]>([])
  const [drawerOpen, setDrawerOpen] = useState(false)
  const [attention, setAttention] = useState<Sin90WeekAttention | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [title, setTitle] = useState('')
  const [windowName, setWindowName] = useState('')
  const [taskTitle, setTaskTitle] = useState('')
  const [note, setNote] = useState('')
  const [blockMinutes, setBlockMinutes] = useState('90')
  const [canWrite, setCanWrite] = useState<boolean | null>(null)
  const [busy, setBusy] = useState(false)
  const seq = useRef(0)

  const load = useCallback((directionId: string | null) => {
    const token = ++seq.current
    setError(null)
    void (async () => {
      try {
        const [nextToday, areas, dirs] = await Promise.all([fetchToday(), fetchAreas(), fetchDirections()])
        if (token !== seq.current) return
        const nextGroups = groupDirections(areas, dirs)
        const nextSelected = directionId && dirs.some((d) => d.id === directionId)
          ? directionId
          : firstDirectionId(nextGroups)
        const [nextTasks, rhythmRows] = nextSelected
          ? await Promise.all([
            fetchTasks(nextSelected),
            Promise.all([fetchWeeks(), fetchBlocks(), fetchRoutines(), fetchRhythms(), fetchProposals(), fetchReviews()]),
          ])
          : [[], [[], [], [], [], [], []]] as const
        if (token !== seq.current) return
        const [nextWeeks, nextBlocks, nextRoutines, nextRhythms, nextProposals, nextReviews] = rhythmRows
        const week = currentWeek(nextWeeks, isoWeekUTC(new Date()))
        const nextAttention = week ? await fetchWeekAttention(week.id) : null
        if (token !== seq.current) return
        setToday(nextToday)
        setGroups(nextGroups)
        setDirections(dirs)
        setSelected(nextSelected)
        setTasks([...nextTasks])
        setWeeks([...nextWeeks])
        setBlocks([...nextBlocks])
        setRoutines([...nextRoutines])
        setRhythms([...nextRhythms])
        setProposals([...nextProposals])
        setReviews([...nextReviews])
        setAttention(nextAttention)
      } catch (err) {
        if (token !== seq.current) return
        setError(err instanceof Error ? err.message : 'load')
      }
    })()
  }, [])

  useEffect(() => { load(null) }, [load])
  const selectedRef = useRef<string | null>(null)
  selectedRef.current = selected
  useEffect(() => {
    const listen = window.agent24.onSin90Event
    if (!listen) return
    return listen((event) => {
      if (sin90ShouldRefresh(event.kind)) load(selectedRef.current)
    })
  }, [load])
  useEffect(() => {
    let live = true
    void writesAvailable().then((ok) => { if (live) setCanWrite(ok) })
    return () => { live = false }
  }, [])

  const enabled = canWrite === true && !busy
  const write = (body: Record<string, unknown>) => {
    if (!enabled) return
    setBusy(true)
    setError(null)
    void sin90Write(body).then(() => {
      if (body['op'] === 'createDirection') { setTitle(''); setWindowName('') }
      if (body['op'] === 'createTask') setTaskTitle('')
      if (body['op'] === 'capture') setNote('')
      load(selected)
    }).catch((err: unknown) => {
      setError(err instanceof Error ? err.message : 'write')
    }).finally(() => setBusy(false))
  }

  // AI only files a proposal. Refresh that list; leave tasks as the last GET left them.
  const suggest = (body: Record<string, unknown>) => {
    if (!enabled) return
    setBusy(true)
    setError(null)
    void sin90Write(body).then(() => fetchProposals()).then((next) => {
      setProposals(next)
      setDrawerOpen(true)
    }).catch((err: unknown) => {
      setError(err instanceof Error ? err.message : 'write')
    }).finally(() => setBusy(false))
  }

  const current = directions.find((d) => d.id === selected) ?? null
  const knownTasks = [...tasks, ...(today?.must_do ?? []), ...(today?.inbox ?? [])]
  const week = currentWeek(weeks, isoWeekUTC(new Date()))
  const reviewId = weeklyDraftReviewId(reviews, isoWeekUTC(new Date()))
  const pending = proposals.filter((proposal) => proposal.status === 'pending')
  const taskName = (id: string) => knownTasks.find((task) => task.id === id)?.title ?? id
  const directionName = (id: string) => directions.find((direction) => direction.id === id)?.title ?? id

  return <div data-sin90-page="">
    {canWrite === false && <p role="status">{t('writesOff')}</p>}
    {error && <p role="alert">{error}</p>}
    {!groups && !error && <p role="status">{t('loading')}</p>}
    {today && <section className="sin90-today" aria-label={t('today')}>
      <div className="sin90-card">
        <h2>{t('mustDo')}</h2>
        {today.must_do.length === 0 ? <p className="sin90-muted">{t('none')}</p> : <ul className="sin90-queue">
          {today.must_do.map((task) => <li key={task.id} className="sin90-row" aria-label={task.title}>
            <span>{task.title}</span>
            <span className="tag">{taskLabel(t, task.status)}</span>
            <TaskActions task={task} enabled={enabled} onWrite={write} />
          </li>)}
        </ul>}
      </div>
      <div className="sin90-card">
        <h2>{t('nextBlock')}</h2>
        {today.deep_block ? <div className="sin90-row">
          <span>
            {t('minutes', { n: today.deep_block.planned_minutes })}
            {blockTitle(today.deep_block, knownTasks) ? ` · ${blockTitle(today.deep_block, knownTasks)}` : ''}
          </span>
          {today.deep_block.status === 'planned' && <button type="button" className="btn btn-ghost" disabled={!enabled}
            onClick={() => { const block = today.deep_block; if (block) write({ op: 'transitionBlock', id: block.id, to: 'started' }) }}>{t('action.started')}</button>}
        </div> : <p className="sin90-muted">{t('none')}</p>}
      </div>
      <div className="sin90-card">
        <h2>{t('inbox')}</h2>
        {today.inbox.length > 0 && <ul className="sin90-queue">
          {today.inbox.map((task) => <li key={task.id}>{task.title}</li>)}
        </ul>}
        <div className="sin90-row">
          <input aria-label={t('inbox')} placeholder={t('capturePlaceholder')} value={note} onChange={(e) => setNote(e.target.value)} />
          <button type="button" className="btn btn-primary" disabled={!enabled || !note.trim()} onClick={() => write({ op: 'capture', text: note })}>{t('capture')}</button>
        </div>
      </div>
    </section>}
    {groups && groups.length === 0 && <div className="sin90-card">
      <p>{t('noDirections')}</p>
      <div className="sin90-fields">
        <label>{t('directionTitle')}<input value={title} onChange={(e) => setTitle(e.target.value)} /></label>
        <label>{t('directionWindow')}<input value={windowName} onChange={(e) => setWindowName(e.target.value)} placeholder="2026-Q4" /></label>
        <button type="button" className="btn btn-primary" disabled={!enabled || !title.trim() || !windowName.trim()}
          onClick={() => write({ op: 'createDirection', title, targetWindow: windowName })}>{t('newDirection')}</button>
      </div>
    </div>}
    {groups && groups.length > 0 && <div className="sin90-body">
      <nav aria-label={t('directions')}>
        {groups.map((group) => <div key={group.areaId ?? 'loose'}>
          <p className="sin90-group">{group.title ?? t('uncategorized')}</p>
          <ul className="sin90-list">
            {group.directions.map((direction) => <li key={direction.id}>
              <button type="button" className="sin90-dir" aria-current={direction.id === selected ? 'true' : undefined}
                onClick={() => load(direction.id)}>{direction.title}</button>
            </li>)}
          </ul>
        </div>)}
      </nav>
      {current && <section aria-label={t('tasks')}>
        <div className="sin90-head">
          <h2>{current.title}</h2>
          <span className="tag">{directionLabel(t, current.status)}</span>
        </div>
        <p className="sin90-muted">{t('window', { window: current.target_window })}</p>
        <Rhythm directionId={current.id} week={week} attention={attention}
          blocks={blocksForDirection(blocks, current.id, tasks)} routines={routinesForDirection(routines, current.id)}
          rhythm={latestRhythm(rhythms)} directions={directions} tasks={tasks} enabled={enabled}
          minutes={blockMinutes} onMinutes={setBlockMinutes} onWrite={write} />
        <div className="sin90-actions">
          <button type="button" className="btn btn-ghost" disabled={!enabled} onClick={() => suggest({ op: 'aiClassify' })}>{t('suggestClassify')}</button>
          {week && (week.status === 'planning' || week.status === 'active') && <button type="button" className="btn btn-ghost" disabled={!enabled}
            onClick={() => suggest({ op: 'aiPropose', weekId: week.id })}>{t('suggestWeek')}</button>}
          {reviewId && <button type="button" className="btn btn-ghost" disabled={!enabled}
            onClick={() => suggest({ op: 'aiSummarize', reviewId })}>{t('suggestReview')}</button>}
        </div>
        <h3>{t('tasks')}</h3>
        {tasks.length === 0 ? <p>{t('noTasks')}</p> : <ul className="sin90-queue">
          {tasks.map((task) => <li key={task.id} className="sin90-row" aria-label={task.title}>
            <span>{task.title}</span>
            <span className="tag">{taskLabel(t, task.status)}</span>
            <TaskActions task={task} enabled={enabled} onWrite={write} />
          </li>)}
        </ul>}
        <div className="sin90-row">
          <input aria-label={t('newTaskTitle')} value={taskTitle} onChange={(e) => setTaskTitle(e.target.value)} />
          <button type="button" className="btn btn-primary" disabled={!enabled || !taskTitle.trim()}
            onClick={() => write({ op: 'createTask', title: taskTitle, directionId: current.id })}>{t('newTask')}</button>
        </div>
      </section>}
    </div>}
    {pending.length > 0 && <div className="sin90-bar">
      <span>{t('proposals', { n: pending.length })}</span>
      <button type="button" className="btn btn-primary" onClick={() => setDrawerOpen((open) => !open)}>
        {drawerOpen ? t('closeProposals') : t('openProposals')}
      </button>
    </div>}
    {pending.length > 0 && drawerOpen && <div className="sin90-drawer" role="dialog" aria-label={t('proposals', { n: pending.length })}>
      {pending.map((proposal) => <article key={proposal.id}>
        <p className="tag">{sourceLabel(proposal.source)}</p>
        {proposal.ops.map((op, index) => <p key={index}>{describeOp(op, taskName, directionName)}</p>)}
        {proposal.rationale && <p className="sin90-muted">{proposal.rationale}</p>}
        <span className="sin90-actions">
          <button type="button" className="btn btn-primary" disabled={!enabled}
            onClick={() => write({ op: 'acceptProposal', id: proposal.id })}>{t('accept')}</button>
          <button type="button" className="btn btn-ghost" disabled={!enabled}
            onClick={() => write({ op: 'rejectProposal', id: proposal.id })}>{t('reject')}</button>
        </span>
      </article>)}
    </div>}
  </div>
}
