import { useCallback, useEffect, useRef, useState } from 'react'
import { Badge, Button, Card, EmptyState, ErrorNotice, PageHeader, SegmentedTabs, Select, TextField } from '../../ui'
import { useT, type T } from '../../i18n'
import { fetchAreas, fetchBlocks, fetchDirections, fetchPeriodPlans, fetchProposals, fetchReviews, fetchRhythms, fetchRoutines, fetchTasks, fetchToday, fetchWeekAttention, fetchWeeks, sin90Write, WriteFailed, writesAvailable } from './api'
import { blockTransitions, currentWeek, firstDirectionId, groupDirections, isoWeekUTC, latestRhythm, routineNext, taskTransitions, weekNext, type DirectionGroup, type Sin90Area, type Sin90Block, type Sin90Direction, type Sin90Proposal, type Sin90Review, type Sin90Task, type Sin90Today, type Sin90Week, type Sin90WeekAttention } from './model'
import { sin90ShouldRefresh } from './refresh'
import { describeOp, sourceLabel } from './sentences'
import { sin90Messages } from './Sin90.i18n'
import './sin90.css'

type Key = keyof typeof sin90Messages.en
type Layer = 'direction' | 'rhythm' | 'today' | 'review'
function taskLabel(t: T<Key>, status: string): string { return status === 'done' ? t('task.done') : status === 'dropped' ? t('task.dropped') : status === 'in_progress' ? t('task.in_progress') : status === 'planned' ? t('task.planned') : t('task.backlog') }
function blockLabel(t: T<Key>, status: string): string { return status === 'planned' ? t('blockPlanned') : status === 'started' ? t('blockStarted') : status === 'completed' ? t('blockCompleted') : status === 'skipped' ? t('blockSkipped') : t('statusUnknown') }
function weekLabel(t: T<Key>, status: string): string { return status === 'planning' ? t('week.planning') : status === 'active' ? t('week.active') : status === 'reviewing' ? t('week.reviewing') : status === 'closed' ? t('week.closed') : t('statusUnknown') }
function reviewLabel(t: T<Key>, status: string): string { return status === 'draft' ? t('reviewDraft') : status === 'pending' ? t('reviewPending') : status === 'completed' || status === 'complete' ? t('reviewCompleted') : status === 'accepted' ? t('reviewAccepted') : t('statusUnknown') }
function reviewKindLabel(t: T<Key>, kind: string): string { return kind === 'weekly' || kind === 'weekly_review' ? t('reviewWeekly') : kind === 'daily' ? t('reviewDaily') : t('reviewOther') }
function impact(op: unknown, t: T<Key>): string {
  if (typeof op !== 'object' || op === null || !('op' in op)) return t('impact.rhythm')
  const name = (op as { op?: unknown }).op
  return name === 'transition_task' ? t('impact.today') : name === 'create_tasks' || name === 'create_task' || name === 'carry_over_task' ? t('impact.week') : t('impact.rhythm')
}
function opKind(op: unknown): unknown { return typeof op === 'object' && op !== null && 'op' in op ? op.op : null }
function periodPlanLabels(data: unknown, period: string): string[] {
  if (typeof data !== 'object' || data === null || !('period_plans' in data) || !Array.isArray(data.period_plans)) return []
  return data.period_plans.flatMap((row): string[] => {
    if (typeof row !== 'object' || row === null) return []
    const cadence = ('period' in row ? row.period : undefined) ?? ('kind' in row ? row.kind : undefined) ?? ('level' in row ? row.level : undefined)
    const label = ('title' in row ? row.title : undefined) ?? ('name' in row ? row.name : undefined)
    return cadence === period && typeof label === 'string' ? [label] : []
  })
}

const tabs: Layer[] = ['direction', 'rhythm', 'today']
export function Sin90Page() {
  const t = useT(sin90Messages)
  const [layer, setLayer] = useState<Layer>('today')
  const [today, setToday] = useState<Sin90Today | null>(null)
  const [areas, setAreas] = useState<Sin90Area[]>([])
  const [groups, setGroups] = useState<DirectionGroup[] | null>(null)
  const [directions, setDirections] = useState<Sin90Direction[]>([])
  const [selected, setSelected] = useState<string | null>(null)
  const [tasks, setTasks] = useState<Sin90Task[]>([])
  const [weeks, setWeeks] = useState<Sin90Week[]>([])
  const [blocks, setBlocks] = useState<Sin90Block[]>([])
  const [routines, setRoutines] = useState<Awaited<ReturnType<typeof fetchRoutines>>>([])
  const [rhythms, setRhythms] = useState<Awaited<ReturnType<typeof fetchRhythms>>>([])
  const [proposals, setProposals] = useState<Sin90Proposal[]>([])
  const [reviews, setReviews] = useState<Sin90Review[]>([])
  const [attention, setAttention] = useState<Sin90WeekAttention | null>(null)
  const [periodPlans, setPeriodPlans] = useState<unknown | null>(null)
  const [failure, setFailure] = useState<string | null>(null)
  const [note, setNote] = useState('')
  const [minutes, setMinutes] = useState('90')
  const [canWrite, setCanWrite] = useState(false)
  const [busy, setBusy] = useState(false)
  const [proposalsOpen, setProposalsOpen] = useState(false)
  const seq = useRef(0)
  const load = useCallback((directionId: string | null) => {
    const token = ++seq.current
    setFailure(null)
    void (async () => {
      try {
        const [td, a, d, pp] = await Promise.all([fetchToday(), fetchAreas(), fetchDirections(), fetchPeriodPlans()])
        if (token !== seq.current) return
        const visibleDirections = d.filter((item) => item.id !== 'sin90-triage')
        const gs = groupDirections(a, visibleDirections)
        const chosen = directionId && d.some((item) => item.id === directionId) ? directionId : firstDirectionId(gs)
        const [ts, data] = chosen ? await Promise.all([fetchTasks(chosen), Promise.all([fetchWeeks(), fetchBlocks(), fetchRoutines(), fetchRhythms(), fetchProposals(), fetchReviews()])]) : [[], [[], [], [], [], [], []]] as const
        const [ws, bs, rs, rys, ps, revs] = data
        const wk = currentWeek(ws, isoWeekUTC(new Date()))
        const att = wk ? await fetchWeekAttention(wk.id).catch(() => null) : null
        if (token !== seq.current) return
        setToday(td); setAreas(a); setGroups(gs); setDirections(visibleDirections); setSelected(chosen); setTasks([...ts]); setWeeks([...ws]); setBlocks([...bs]); setRoutines([...rs]); setRhythms([...rys]); setProposals([...ps]); setReviews([...revs]); setAttention(att); setPeriodPlans(pp)
      } catch (err) { if (token === seq.current) setFailure(err instanceof Error ? err.message : 'Could not load Sin90.') }
    })()
  }, [])
  useEffect(() => { load(null) }, [load])
  const selectedRef = useRef<string | null>(null); selectedRef.current = selected
  useEffect(() => { const listen = window.agent24.onSin90Event; if (!listen) return; return listen((event) => { if (sin90ShouldRefresh(event.kind)) load(selectedRef.current) }) }, [load])
  useEffect(() => { let live = true; void writesAvailable().then((available) => { if (live) setCanWrite(available) }); return () => { live = false } }, [])
  const write = (body: Record<string, unknown>) => {
    if (!canWrite || busy) return
    setBusy(true); setFailure(null)
    void sin90Write(body).then(() => { if (body['op'] === 'capture') setNote(''); load(selected) }).catch((err: unknown) => { setFailure(err instanceof WriteFailed && err.code === 'module_delegation_required' ? t('error.moduleDelegation') : err instanceof Error ? err.message : t('loadError')) }).finally(() => setBusy(false))
  }
  const enabled = canWrite && !busy
  const direction = directions.find((item) => item.id === selected) ?? null
  const week = currentWeek(weeks, isoWeekUTC(new Date()))
  const attentionText = attention ? t('attention', { planned_min: attention.planned_min, actual_min: attention.actual_min, deviation_min: attention.deviation_min }) : null
  const pending = proposals.filter((proposal) => proposal.status === 'pending')
  const taskTitle = (id: string) => [...tasks, ...(today?.must_do ?? []), ...(today?.inbox ?? [])].find((task) => task.id === id)?.title ?? id
  const dirTitle = (id: string) => directions.find((item) => item.id === id)?.title ?? id
  const layerName = (value: Layer) => value === 'direction' ? t('layer.direction') : value === 'rhythm' ? t('layer.rhythm') : t('today')
  const acceptAll = () => { for (const proposal of pending) write({ op: 'acceptProposal', id: proposal.id }) }
  const completeTask = (task: Sin90Task) => {
    const paths: Record<string, string[]> = { backlog: ['planned', 'in_progress', 'done'], planned: ['in_progress', 'done'], in_progress: ['done'] }
    const path = paths[task.status]
    if (!path) { setFailure(t('taskNoCompletePath')); return }
    setBusy(true); setFailure(null)
    void (async () => { try { for (const to of path) await sin90Write({ op: 'transitionTask', id: task.id, to }); load(selected) } catch (err) { setFailure(err instanceof Error ? err.message : t('loadError')) } finally { setBusy(false) } })()
  }
  const capture = () => { if (note.trim()) write({ op: 'capture', text: note }) }
  const reviewMode = layer === 'review'

  // Rhythm is the whole week: every block, not one direction's.
  const weekBlocks = blocks
  function renderWeek() { return <>
    {week ? <p>{t('thisWeek', { week: week.iso_week })} · {weekLabel(t, week.status)}</p> : <p className="sin90-muted">{t('noWeek')}</p>}
    {attentionText && <p>{attentionText}</p>}
    <ul className="sin90-queue sin90-block-queue">{weekBlocks.map((block, index) => <li key={block.id}><span><strong>{block.task_id ? taskTitle(block.task_id) : dirTitle(block.direction_id ?? '')}</strong><small>{block.direction_id ? dirTitle(block.direction_id) : ''} · {block.planned_minutes} {t('minuteUnit')} · {t('queueBlock', { index: index + 1 })}</small></span><Badge>{blockLabel(t, block.status)}</Badge><span className="sin90-row-actions">{blockTransitions(block.status).map((to) => <Button key={to} size="sm" disabled={!enabled} onClick={() => write({ op: 'transitionBlock', id: block.id, to })}>{to === 'completed' ? t('action.completed') : to === 'started' ? t('action.started') : t('action.skipped')}</Button>)}</span></li>)}</ul>
    <div className="sin90-add-block"><TextField label={t('blockMinutes')} inputMode="numeric" value={minutes} onChange={(event) => setMinutes(event.target.value)} /><Select label={t('blockDirection')} value={selected ?? ''} onChange={(event) => setSelected(event.target.value)}>{directions.map((item) => <option key={item.id} value={item.id}>{item.title}</option>)}</Select><Button variant="primary" disabled={!enabled || !Number.isInteger(Number(minutes)) || Number(minutes) < 1 || !direction} onClick={() => direction && write({ op: 'createBlock', directionId: direction.id, plannedMinutes: Number(minutes) })}>{t('addBlock')}</Button></div>
    {routines.filter((routine) => routine.status !== 'retired').map((routine) => { const next = routineNext(routine.status); return <p key={routine.id}>{routine.title} {next && <Button size="sm" disabled={!enabled} onClick={() => write({ op: 'transitionRoutine', id: routine.id, to: next })}>{next === 'paused' ? t('pause') : t('resume')}</Button>}</p> })}
    {latestRhythm(rhythms) && <p>{t('rhythm')}: {directions.map((item) => `${item.title} ${latestRhythm(rhythms)?.allocations.find((row) => row.direction_id === item.id)?.pct ?? 0}%`).join(' · ')}</p>}
  </>}

  return <main className="sin90-page">
    <PageHeader title={t('pageTitle')} actions={<><SegmentedTabs label={t('layers')} tabs={tabs.map((value) => ({ value, label: layerName(value) }))} value={reviewMode ? '' : layer} onChange={(value) => { if (tabs.includes(value as Layer)) setLayer(value as Layer) }} /><Button variant={reviewMode ? 'primary' : 'secondary'} onClick={() => setLayer('review')} aria-label={t('review')}>↺ {t('review')}</Button></>} />
    {failure && <ErrorNotice message={failure} actionLabel={t('retry')} onAction={() => load(selected)} />}
    {!canWrite && <p className="sin90-muted" role="status">{t('writesOff')}</p>}
    {!groups && !failure && <p role="status">{t('loading')}</p>}

    {layer === 'today' && today && <div className="sin90-view">
      <p className="sin90-capture-hint">{t('captureHint')}</p><form className="sin90-capture" onSubmit={(event) => { event.preventDefault(); capture() }}><TextField label={t('captureLabel')} placeholder={t('capturePlaceholder')} value={note} onChange={(event) => setNote(event.target.value)} /><Button type="submit" variant="primary" aria-label={t('capture')} disabled={!enabled || !note.trim()}>{t('capture')} ↵</Button></form>
      <div className="sin90-columns">
        <section className="sin90-stack">
          <Card title={t('nextBlock')}><p className="sin90-muted">{today.deep_block ? <><strong>{today.deep_block.task_id ? taskTitle(today.deep_block.task_id) : dirTitle(today.deep_block.direction_id ?? '')}</strong><br />{today.deep_block.direction_id ? dirTitle(today.deep_block.direction_id) : tasks.find((task) => task.id === today.deep_block?.task_id)?.direction_id ? dirTitle(tasks.find((task) => task.id === today.deep_block?.task_id)?.direction_id ?? '') : ''} · {today.deep_block.planned_minutes} {t('minuteUnit')} · {t('queueBlock', { index: Math.max(1, blocks.findIndex((b) => b.id === today.deep_block?.id) + 1) })}</> : t('emptyFocus')}</p>{today.deep_block?.status === 'planned' && <Button variant="primary" disabled={!enabled} onClick={() => write({ op: 'transitionBlock', id: today.deep_block?.id, to: 'started' })}>{t('action.started')}</Button>}</Card>
          <Card title={t('mustDo')}><ul className="sin90-queue">{today.must_do.map((task) => <li key={task.id} aria-label={task.title} className="sin90-task-row"><button type="button" className="sin90-check" aria-label={taskLabel(t, task.status) === t('task.done') ? t('task.done') : t('action.done')} disabled={!enabled || task.status === 'done'} onClick={() => completeTask(task)}>{task.status === 'done' ? '✓' : ''}</button><span className={task.status === 'done' ? 'sin90-task-done' : ''}>{task.title}</span><Badge tone="accent">{directions.find((item) => item.id === task.direction_id)?.title ?? t('uncategorized')}</Badge><details className="sin90-menu"><summary aria-label={t('taskActions')}>⋯</summary><div>{taskTransitions(task.status).filter((to) => to !== 'done').map((to) => <button key={to} type="button" onClick={() => write({ op: 'transitionTask', id: task.id, to })}>{to === 'planned' ? t('taskSchedule') : to === 'in_progress' ? t('taskStart') : to === 'dropped' ? t('taskDrop') : t('action.done')}</button>)}</div></details></li>)}</ul>{today.must_do.length === 0 && <EmptyState description={t('none')} action={null} />}</Card>
          <Card title={t('inbox')}><ul className="sin90-queue">{today.inbox.map((task) => <li key={task.id}>{task.title}</li>)}</ul>{today.inbox.length === 0 && <EmptyState description={t('none')} action={null} />}{pending.some((p) => p.ops.some((op) => opKind(op) === 'assign_task_direction')) && <div className="sin90-actions"><Badge tone="accent">{t('classified')}</Badge><Button variant="primary" disabled={!enabled} onClick={acceptAll}>{t('acceptAll')}</Button><Button onClick={() => { setLayer('today'); setProposalsOpen(true) }}>{t('reviewOneByOne')}</Button></div>}</Card>
        </section>
        <aside className="sin90-stack"><Card title={t('weekSummary')} actions={<Button size="sm" onClick={() => setLayer('rhythm')}>{t('seeWeek')}</Button>}>{week ? <><p>{t('thisWeek', { week: week.iso_week })}</p><p>{attentionText ?? t('attentionUnavailable')}</p></> : <p className="sin90-muted">{t('noWeek')}</p>}</Card><Card title={t('directions')}><ul className="sin90-queue">{directions.slice(0, 4).map((item) => <li key={item.id}>{item.title}<Badge tone="accent">{item.target_window === 'n/a' ? t('window.na') : item.target_window === 'ongoing' ? t('window.ongoing') : item.target_window}</Badge></li>)}</ul>{directions.length === 0 && <EmptyState description={t('noDirections')} action={null} />}</Card></aside>
      </div>
    </div>}

    {layer === 'direction' && groups && <div className="sin90-view"><div className="sin90-intro"><h2>{t('systemsTitle')}</h2></div>{areas.length === 0 && <Card title={t('systemsEmptyTitle')}><p>{t('systemsEmpty')}</p><Button variant="primary" disabled={!enabled} onClick={() => write({ op: 'installPack' })}>{t('installSystems')}</Button></Card>}<div className="sin90-area-grid">{groups.map((group) => <Card key={group.areaId ?? 'loose'} title={group.title ?? t('uncategorized')}>{group.areaId ? (() => { const area = areas.find((item) => item.id === group.areaId); const goals = group.directions.map((item) => item.goal_3y).filter((goal): goal is string => Boolean(goal)); return area?.current_state || area?.ideal_state || goals.length ? <div className="sin90-detail">{(area?.current_state || area?.ideal_state) && <p>{t('nowIdeal')}: {area?.current_state ?? ''} → {area?.ideal_state ?? ''}</p>}{goals.length > 0 && <p>{t('threeYearGoal')}: {goals.join(' · ')}</p>}</div> : null })() : null}<ul className="sin90-queue">{group.directions.map((item) => <li key={item.id}><button className="sin90-link" onClick={() => setSelected(item.id)}>{item.title}</button><Badge>{item.target_window === 'n/a' ? t('window.na') : item.target_window === 'ongoing' ? t('window.ongoing') : item.target_window}</Badge><span>{item.goal_3y ?? ''}</span></li>)}</ul><p className="sin90-v06-note">{t('v06Placeholder')}</p></Card>)}</div>{groups.length === 0 && areas.length > 0 && <EmptyState description={t('noDirections')} action={null} />}</div>}

    {layer === 'rhythm' && <div className="sin90-view">
      {periodPlans ? <div className="sin90-period-columns">{(['year', 'half', 'quarter', 'month'] as const).map((period) => { const labels = periodPlanLabels(periodPlans, period); return <Card key={period} title={t(`period.${period}`)}>{labels.length ? <ul className="sin90-queue">{labels.map((label) => <li key={label}>{label}</li>)}</ul> : <p className="sin90-muted">{t('none')}</p>}</Card> })}<Card title={t('weekColumn')}>{renderWeek()}</Card></div> : <><p className="sin90-period-note">{t('periodNote')}</p><Card title={t('weekColumn')} actions={week && weekNext(week.status) ? <Button size="sm" variant="primary" disabled={!enabled} onClick={() => write({ op: 'transitionWeek', id: week.id, to: weekNext(week.status) })}>{weekNext(week.status) === 'active' ? t('startWeek') : weekNext(week.status) === 'reviewing' ? t('reviewWeek') : t('closeWeek')}</Button> : !week ? <Button size="sm" variant="primary" disabled={!enabled} onClick={() => write({ op: 'createWeek', isoWeek: isoWeekUTC(new Date()) })}>{t('openWeek')}</Button> : undefined}>{renderWeek()}</Card></>}
    </div>}

    {layer === 'review' && <div className="sin90-columns sin90-view"><section className="sin90-stack"><Card title={t('recentReviews')}>{reviews.length === 0 ? <EmptyState description={t('none')} action={null} /> : reviews.map((review) => <article className="sin90-proposal" key={review.id}><div className="sin90-row"><Badge>{reviewKindLabel(t, review.kind)}</Badge><span>{review.period}</span><Badge>{reviewLabel(t, review.status)}</Badge></div></article>)}{pending.map((proposal) => <article className="sin90-proposal" key={proposal.id}><div className="sin90-row"><Badge tone="accent">{sourceLabel(proposal.source)}</Badge>{proposal.ops.map((op) => impact(op, t)).map((text, index) => <Badge key={index}>{text}</Badge>)}</div>{proposal.ops.map((op, index) => <p key={index}>{describeOp(op, taskTitle, dirTitle)}</p>)}{proposal.rationale && <p className="sin90-muted">{proposal.rationale}</p>}<Button variant="primary" disabled={!enabled} onClick={() => write({ op: 'acceptProposal', id: proposal.id })}>{t('accept')}</Button><Button disabled={!enabled} onClick={() => write({ op: 'rejectProposal', id: proposal.id })}>{t('reject')}</Button></article>)}</Card></section><aside className="sin90-stack"><Card title={t('autoReview')}><p>{t('everySunday')}</p><p className="sin90-muted">{t('v06Placeholder')}</p></Card><Card title={t('reviewImpact')}><p>{t('impact.today')} · {t('impact.week')} · {t('impact.rhythm')}</p></Card></aside></div>}
    {proposalsOpen && <div className="sin90-view"><Card title={t('proposals')} actions={<Button onClick={() => setProposalsOpen(false)}>{t('close')}</Button>}>{pending.map((proposal) => <article className="sin90-proposal" key={proposal.id}><Badge tone="accent">{sourceLabel(proposal.source)}</Badge>{proposal.ops.map((op, index) => <p key={index}><Badge>{impact(op, t)}</Badge> {describeOp(op, taskTitle, dirTitle)}</p>)}{proposal.rationale && <p className="sin90-muted">{proposal.rationale}</p>}<Button variant="primary" disabled={!enabled} onClick={() => write({ op: 'acceptProposal', id: proposal.id })}>{t('accept')}</Button><Button disabled={!enabled} onClick={() => write({ op: 'rejectProposal', id: proposal.id })}>{t('reject')}</Button></article>)}</Card></div>}
  </main>
}
