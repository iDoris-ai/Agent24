import { useCallback, useEffect, useRef, useState } from 'react'
import { useT, type T } from '../../i18n'
import { fetchAreas, fetchDirections, fetchTasks, fetchToday, sin90Write, writesAvailable } from './api'
import { canCarry, firstDirectionId, groupDirections, nextIsoWeek, taskTransitions, type DirectionGroup, type Sin90Block, type Sin90Direction, type Sin90Task, type Sin90Today } from './model'
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

export function Sin90Page() {
  const t = useT(sin90Messages)
  const [today, setToday] = useState<Sin90Today | null>(null)
  const [groups, setGroups] = useState<DirectionGroup[] | null>(null)
  const [directions, setDirections] = useState<Sin90Direction[]>([])
  const [selected, setSelected] = useState<string | null>(null)
  const [tasks, setTasks] = useState<Sin90Task[]>([])
  const [error, setError] = useState<string | null>(null)
  const [title, setTitle] = useState('')
  const [windowName, setWindowName] = useState('')
  const [taskTitle, setTaskTitle] = useState('')
  const [note, setNote] = useState('')
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
        const nextTasks = nextSelected ? await fetchTasks(nextSelected) : []
        if (token !== seq.current) return
        setToday(nextToday)
        setGroups(nextGroups)
        setDirections(dirs)
        setSelected(nextSelected)
        setTasks(nextTasks)
      } catch (err) {
        if (token !== seq.current) return
        setError(err instanceof Error ? err.message : 'load')
      }
    })()
  }, [])

  useEffect(() => { load(null) }, [load])
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

  const current = directions.find((d) => d.id === selected) ?? null
  const knownTasks = [...tasks, ...(today?.must_do ?? []), ...(today?.inbox ?? [])]

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
        {today.deep_block ? <p>
          {t('minutes', { n: today.deep_block.planned_minutes })}
          {blockTitle(today.deep_block, knownTasks) ? ` · ${blockTitle(today.deep_block, knownTasks)}` : ''}
        </p> : <p className="sin90-muted">{t('none')}</p>}
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
  </div>
}
