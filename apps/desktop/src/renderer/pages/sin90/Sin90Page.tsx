import { useCallback, useEffect, useRef, useState } from 'react'
import { Badge, Button, Card, EmptyState, ErrorNotice, PageHeader, SegmentedTabs, Select, TextField } from '../../ui'
import { useLocale, useT, type T } from '../../i18n'
import {
  exportToFile, fetchAreas, fetchBlocks, fetchDataStatus, fetchDirections, fetchPlans,
  fetchProgress, fetchProposals, fetchReviews, fetchReviewSettings, fetchRhythms,
  fetchRoutines, fetchTasks, fetchToday, fetchWeekAttention, fetchWeeks,
  importFromFile, sin90Write, WriteFailed, writesAvailable,
} from './api'
import {
  blockTransitions, currentWeek, firstDirectionId, groupDirections, isoWeekUTC,
  latestRhythm, legalPlanParents, routineNext, taskTransitions, weekNext,
  type DirectionGroup, type Sin90Area, type Sin90Block, type Sin90Direction,
  type Sin90Proposal, type Sin90Review, type Sin90Task, type Sin90Today,
  type Sin90Week, type Sin90WeekAttention, type Sin90PeriodPlan,
  type Sin90PlanProgress, type Sin90ReviewSettings, type Sin90DataStatus,
} from './model'
import { sin90ShouldRefresh } from './refresh'
import { describeOp, sourceLabel } from './sentences'
import { sin90Messages } from './Sin90.i18n'
import './sin90.css'

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

interface ConfirmState {
  title: string
  message: string
  onConfirm: () => void
}

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
function progressLabel(t: T<Key>, status: string): string { return status === 'at_risk' ? t('progress.risk') : status === 'behind' ? t('progress.behind') : t('progress.ok') }

const tabs: Layer[] = ['direction', 'rhythm', 'today']
const DELEGATION_CODES = new Set([
  'module_delegation_required',
  'module_delegation_denied',
  'presence_denied',
  'presence_timeout',
  'presence_unavailable',
])

export function Sin90Page() {
  const t = useT(sin90Messages)
  const currentLocale = useLocale()
  const initialLang = (currentLocale === 'zh' || currentLocale === 'en' || currentLocale === 'th') ? currentLocale : 'zh'
  const [demoLang, setDemoLang] = useState<'zh' | 'en' | 'th'>(initialLang)
  useEffect(() => {
    if (currentLocale === 'zh' || currentLocale === 'en' || currentLocale === 'th') {
      setDemoLang(currentLocale)
    }
  }, [currentLocale])

  const [dataStatus, setDataStatus] = useState<Sin90DataStatus | null>(null)
  const [bannerDismissed, setBannerDismissed] = useState<boolean>(() => {
    try {
      return localStorage.getItem('sin90:demo-banner-dismissed') === '1'
    } catch {
      return false
    }
  })
  const [dataPanelOpen, setDataPanelOpen] = useState(false)
  const [confirmAction, setConfirmAction] = useState<ConfirmState | null>(null)

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
  const [periodPlans, setPeriodPlans] = useState<Sin90PeriodPlan[]>([])
  const [progress, setProgress] = useState<Sin90PlanProgress[]>([])
  const [reviewSettings, setReviewSettings] = useState<Sin90ReviewSettings>({ auto: true, cadence: 'weekly', weekday: 7, hour: 18 })
  const [planKind, setPlanKind] = useState<'year' | 'half' | 'quarter' | 'month'>('year')
  const [planPeriod, setPlanPeriod] = useState(String(new Date().getFullYear()))
  const [planParent, setPlanParent] = useState('')
  const [planTarget, setPlanTarget] = useState('')
  const [planMetric, setPlanMetric] = useState<'none' | 'count' | 'minutes'>('none')
  const [planMetricTarget, setPlanMetricTarget] = useState('')
  const [planEdits, setPlanEdits] = useState<Record<string, { target: string; metricTarget: string }>>({})
  const [areaEditing, setAreaEditing] = useState<Record<string, boolean>>({})
  const [areaDrafts, setAreaDrafts] = useState<Record<string, { current: string; ideal: string }>>({})
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
        const [td, a, d, pp, ds] = await Promise.all([
          fetchToday(),
          fetchAreas(),
          fetchDirections(),
          fetchPlans(),
          fetchDataStatus().catch(() => null),
        ])
        if (token !== seq.current) return
        const visibleDirections = d.filter((item) => item.id !== 'sin90-triage')
        const gs = groupDirections(a, visibleDirections)
        const chosen = directionId && d.some((item) => item.id === directionId) ? directionId : firstDirectionId(gs)
        // Week data loads even with no direction yet, so a new user can open a week.
        const [ts, data, progressRows, settings] = await Promise.all([chosen ? fetchTasks(chosen) : Promise.resolve([] as Awaited<ReturnType<typeof fetchTasks>>), Promise.all([fetchWeeks(), fetchBlocks(), fetchRoutines(), fetchRhythms(), fetchProposals(), fetchReviews()]), Promise.all(visibleDirections.map((item) => fetchProgress(item.id).catch(() => []))).then((rows) => rows.flat()), fetchReviewSettings().catch(() => ({ auto: true, cadence: 'weekly' as const, weekday: 7, hour: 18 }))])
        const [ws, bs, rs, rys, ps, revs] = data
        const wk = currentWeek(ws, isoWeekUTC(new Date()))
        const att = wk ? await fetchWeekAttention(wk.id).catch(() => null) : null
        if (token !== seq.current) return
        setToday(td); setAreas(a); setGroups(gs); setDirections(visibleDirections); setSelected(chosen); setTasks([...ts]); setWeeks([...ws]); setBlocks([...bs]); setRoutines([...rs]); setRhythms([...rys]); setProposals([...ps]); setReviews([...revs]); setAttention(att); setPeriodPlans(pp); setProgress(progressRows); setReviewSettings(settings)
        if (ds) setDataStatus(ds)
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
    void sin90Write(body).then(() => { if (body['op'] === 'capture') setNote(''); load(selected) }).catch((err: unknown) => { setFailure(err instanceof WriteFailed && DELEGATION_CODES.has(err.code) ? t('error.moduleDelegation') : err instanceof Error ? err.message : t('loadError')) }).finally(() => setBusy(false))
  }
  const enabled = canWrite && !busy
  const direction = directions.find((item) => item.id === selected) ?? null
  const week = currentWeek(weeks, isoWeekUTC(new Date()))
  const attentionText = attention ? t('attention', { planned_min: attention.planned_min, actual_min: attention.actual_min, deviation_min: attention.deviation_min }) : null
  const pending = proposals.filter((proposal) => proposal.status === 'pending')
  const taskTitle = (id: string) => [...tasks, ...(today?.must_do ?? []), ...(today?.inbox ?? [])].find((task) => task.id === id)?.title ?? id
  const dirTitle = (id: string) => directions.find((item) => item.id === id)?.title ?? id
  const layerName = (value: Layer) => value === 'direction' ? t('layer.direction') : value === 'rhythm' ? t('layer.rhythm') : t('today')
  // One at a time: `write` guards on `busy`, so a loop of writes would race.
  const acceptAll = () => {
    if (!canWrite || busy) return
    setBusy(true); setFailure(null)
    void (async () => { try { for (const proposal of pending) await sin90Write({ op: 'acceptProposal', id: proposal.id }) } catch (err) { setFailure(err instanceof WriteFailed && DELEGATION_CODES.has(err.code) ? t('error.moduleDelegation') : err instanceof Error ? err.message : t('loadError')) } finally { setBusy(false); load(selected) } })()
  }
  const completeTask = (task: Sin90Task) => {
    const paths: Record<string, string[]> = { backlog: ['planned', 'in_progress', 'done'], planned: ['in_progress', 'done'], in_progress: ['done'] }
    const path = paths[task.status]
    if (!path) { setFailure(t('taskNoCompletePath')); return }
    setBusy(true); setFailure(null)
    void (async () => { try { for (const to of path) await sin90Write({ op: 'transitionTask', id: task.id, to }) } catch (err) { setFailure(err instanceof WriteFailed && DELEGATION_CODES.has(err.code) ? t('error.moduleDelegation') : err instanceof Error ? err.message : t('loadError')) } finally { setBusy(false); load(selected) } })()
  }
  const capture = () => { if (note.trim()) write({ op: 'capture', text: note }) }
  const reviewMode = layer === 'review'

  const onBannerClear = () => {
    setConfirmAction({
      title: t('demoBannerClear'),
      message: t('demoBannerClearConfirm'),
      onConfirm: () => {
        setConfirmAction(null)
        write({ op: 'dataReset' })
      },
    })
  }

  const dismissBanner = () => {
    setBannerDismissed(true)
    try {
      localStorage.setItem('sin90:demo-banner-dismissed', '1')
    } catch {
      // ignore
    }
  }

  const handleExport = () => {
    if (!canWrite || busy) return
    setBusy(true)
    setFailure(null)
    void exportToFile()
      .catch((err: unknown) => {
        setFailure(err instanceof WriteFailed && DELEGATION_CODES.has(err.code) ? t('error.moduleDelegation') : err instanceof Error ? err.message : t('loadError'))
      })
      .finally(() => setBusy(false))
  }

  const handleImport = () => {
    setConfirmAction({
      title: t('dataPanelImport'),
      message: t('dataPanelImportConfirm'),
      onConfirm: () => {
        setConfirmAction(null)
        if (!canWrite || busy) return
        setBusy(true)
        setFailure(null)
        void importFromFile()
          .then(() => load(selected))
          .catch((err: unknown) => {
            setFailure(err instanceof WriteFailed && DELEGATION_CODES.has(err.code) ? t('error.moduleDelegation') : err instanceof Error ? err.message : t('loadError'))
          })
          .finally(() => setBusy(false))
      },
    })
  }

  const handleRestoreBackup = (name: string) => {
    setConfirmAction({
      title: t('dataPanelRestoreBackup'),
      message: t('dataPanelRestoreBackupConfirm', { name }),
      onConfirm: () => {
        setConfirmAction(null)
        write({ op: 'dataRestoreBackup', name })
      },
    })
  }

  const handleLoadDemo = () => {
    setConfirmAction({
      title: t('dataPanelLoadDemoAction'),
      message: t('dataPanelLoadDemoConfirm'),
      onConfirm: () => {
        setConfirmAction(null)
        setBannerDismissed(false)
        try { localStorage.removeItem('sin90:demo-banner-dismissed') } catch { /* ignore */ }
        write({ op: 'dataReset', template: 'graduate', lang: demoLang })
      },
    })
  }

  const handleSwitchDemoLanguage = () => {
    write({ op: 'dataReset', template: 'graduate', lang: currentLocale })
  }

  const handleClear = () => {
    setConfirmAction({
      title: t('dataPanelClearAction'),
      message: t('dataPanelClearConfirm'),
      onConfirm: () => {
        setConfirmAction(null)
        write({ op: 'dataReset' })
      },
    })
  }

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
    <PageHeader
      title={t('pageTitle')}
      actions={
        <>
          <SegmentedTabs label={t('layers')} tabs={tabs.map((value) => ({ value, label: layerName(value) }))} value={reviewMode ? '' : layer} onChange={(value) => { if (tabs.includes(value as Layer)) setLayer(value as Layer) }} />
          <Button variant={reviewMode ? 'primary' : 'secondary'} onClick={() => setLayer('review')} aria-label={t('review')}>↺ {t('review')}</Button>
          <Button variant={dataPanelOpen ? 'primary' : 'secondary'} onClick={() => setDataPanelOpen((v) => !v)} aria-label={t('dataPanelButton')}>{t('dataPanelButton')}</Button>
        </>
      }
    />
    {failure && <ErrorNotice message={failure} actionLabel={t('retry')} onAction={() => load(selected)} />}
    {!canWrite && <p className="sin90-muted" role="status">{t('writesOff')}</p>}
    {!groups && !failure && <p role="status">{t('loading')}</p>}

    {dataStatus?.template && !bannerDismissed && (
      <aside className="sin90-demo-banner" aria-label={t('demoBannerTitle', { title: dataStatus.template.title })}>
        <div className="sin90-demo-banner-header">
          <span className="sin90-demo-banner-title">{t('demoBannerTitle', { title: dataStatus.template.title })}</span>
        </div>
        <p className="sin90-demo-banner-guide">{dataStatus.template.guide}</p>
        <div className="sin90-demo-banner-actions">
          {dataStatus.template.lang !== currentLocale && (currentLocale === 'zh' || currentLocale === 'en' || currentLocale === 'th') && <Button variant="primary" size="sm" disabled={!enabled} onClick={handleSwitchDemoLanguage}>{t('demoBannerSwitchLanguage', { language: currentLocale === 'zh' ? '中文' : currentLocale === 'en' ? 'English' : 'ไทย' })}</Button>}
          <Button variant="primary" size="sm" disabled={!enabled} onClick={onBannerClear}>
            {t('demoBannerClear')}
          </Button>
          <Button size="sm" onClick={dismissBanner}>
            {t('demoBannerDismiss')}
          </Button>
        </div>
      </aside>
    )}

    {confirmAction && !dataPanelOpen && (
      <div className="sin90-confirm-card" role="alert">
        <p><strong>{confirmAction.title}</strong></p>
        <p>{confirmAction.message}</p>
        <div className="sin90-confirm-actions">
          <Button variant="danger" size="sm" disabled={!enabled} onClick={confirmAction.onConfirm}>
            {t('dataPanelConfirmBtn')}
          </Button>
          <Button variant="secondary" size="sm" disabled={!enabled} onClick={() => setConfirmAction(null)}>
            {t('dataPanelCancelBtn')}
          </Button>
        </div>
      </div>
    )}

    {dataPanelOpen && (
      <div className="sin90-view">
        <Card title={t('dataPanelTitle')} actions={<Button size="sm" onClick={() => setDataPanelOpen(false)}>{t('dataPanelClose')}</Button>} className="sin90-data-panel-card">
          {confirmAction && (
            <div className="sin90-confirm-card" role="alert">
              <p><strong>{confirmAction.title}</strong></p>
              <p>{confirmAction.message}</p>
              <div className="sin90-confirm-actions">
                <Button variant="danger" size="sm" disabled={!enabled} onClick={confirmAction.onConfirm}>
                  {t('dataPanelConfirmBtn')}
                </Button>
                <Button variant="secondary" size="sm" disabled={!enabled} onClick={() => setConfirmAction(null)}>
                  {t('dataPanelCancelBtn')}
                </Button>
              </div>
            </div>
          )}

          <section className="sin90-data-section">
            <h3>{t('dataPanelSummaryTitle')}</h3>
            <p>
              {t('dataPanelCounts', {
                areas: dataStatus?.counts.areas ?? 0,
                directions: dataStatus?.counts.directions ?? 0,
                plans: dataStatus?.counts.plans ?? 0,
                tasks: dataStatus?.counts.tasks ?? 0,
              })}
            </p>
            <p className="sin90-muted">
              {dataStatus?.template
                ? t('dataPanelIsDemo', { title: dataStatus.template.title, time: dataStatus.template.loaded_at })
                : t('dataPanelIsNotDemo')}
            </p>
          </section>

          <section className="sin90-data-section">
            <div className="sin90-row-actions">
              <Button variant="secondary" disabled={!enabled} onClick={handleExport}>
                {t('dataPanelExport')}
              </Button>
              <Button variant="secondary" disabled={!enabled} onClick={handleImport}>
                {t('dataPanelImport')}
              </Button>
            </div>
            <p className="sin90-muted">{t('dataPanelExportHint')}</p>
          </section>

          <section className="sin90-data-section">
            <h3>{t('dataPanelBackupsTitle')}</h3>
            {(!dataStatus?.backups || dataStatus.backups.length === 0) ? (
              <p className="sin90-muted">{t('dataPanelBackupsNone')}</p>
            ) : (
              <ul className="sin90-queue">
                {dataStatus.backups.map((b) => (
                  <li key={b.name} className="sin90-backup-row">
                    <span>
                      <strong>{b.name}</strong>
                      <br />
                      <small>{b.at} · {formatBytes(b.bytes)}</small>
                    </span>
                    <Button size="sm" disabled={!enabled} onClick={() => handleRestoreBackup(b.name)}>
                      {t('dataPanelRestoreBackup')}
                    </Button>
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section className="sin90-data-section">
            <h3>{t('dataPanelLoadDemoTitle')}</h3>
            <div className="sin90-detail" style={{ display: 'flex', alignItems: 'end', gap: 12 }}>
              <Select
                id="sin90-demo-lang"
                label={t('dataPanelLoadDemoLanguage')}
                value={demoLang}
                onChange={(e) => setDemoLang(e.target.value as 'zh' | 'en' | 'th')}
              >
                <option value="zh">中文 (zh)</option>
                <option value="en">English (en)</option>
                <option value="th">ไทย (th)</option>
              </Select>
              <Button disabled={!enabled} onClick={handleLoadDemo}>
                {t('dataPanelLoadDemoAction')}
              </Button>
            </div>
          </section>

          <section className="sin90-data-section">
            <h3>{t('dataPanelClearTitle')}</h3>
            <div>
              <Button variant="danger" disabled={!enabled} onClick={handleClear}>
                {t('dataPanelClearAction')}
              </Button>
            </div>
          </section>
        </Card>
      </div>
    )}

    {layer === 'today' && today && <div className="sin90-view">
      <p className="sin90-capture-hint">{t('captureHint')}</p><form className="sin90-capture" onSubmit={(event) => { event.preventDefault(); capture() }}><TextField label={t('captureLabel')} placeholder={t('capturePlaceholder')} value={note} onChange={(event) => setNote(event.target.value)} /><Button type="submit" variant="primary" aria-label={t('capture')} disabled={!enabled || !note.trim()}>{t('capture')} ↵</Button></form>
      <div className="sin90-columns">
        <section className="sin90-stack">
          <Card title={t('nextBlock')}><p className="sin90-muted">{today.deep_block ? <><strong>{today.deep_block.task_id ? taskTitle(today.deep_block.task_id) : dirTitle(today.deep_block.direction_id ?? '')}</strong><br />{today.deep_block.direction_id ? dirTitle(today.deep_block.direction_id) : tasks.find((task) => task.id === today.deep_block?.task_id)?.direction_id ? dirTitle(tasks.find((task) => task.id === today.deep_block?.task_id)?.direction_id ?? '') : ''} · {today.deep_block.planned_minutes} {t('minuteUnit')} · {t('queueBlock', { index: Math.max(1, blocks.findIndex((b) => b.id === today.deep_block?.id) + 1) })}</> : t('emptyFocus')}</p>{today.deep_block?.status === 'planned' && <Button variant="primary" disabled={!enabled} onClick={() => write({ op: 'transitionBlock', id: today.deep_block?.id, to: 'started' })}>{t('action.started')}</Button>}</Card>
          <Card title={t('mustDo')}><ul className="sin90-queue">{today.must_do.map((task) => <li key={task.id} aria-label={task.title} className="sin90-task-row"><button type="button" className="sin90-check" aria-label={taskLabel(t, task.status) === t('task.done') ? t('task.done') : t('action.done')} disabled={!enabled || task.status === 'done'} onClick={() => completeTask(task)}>{task.status === 'done' ? '✓' : ''}</button><span className={task.status === 'done' ? 'sin90-task-done' : ''}>{task.title}</span><span className="sin90-task-direction" title={directions.find((item) => item.id === task.direction_id)?.title ?? t('uncategorized')}>{directions.find((item) => item.id === task.direction_id)?.title ?? t('uncategorized')}</span><details className="sin90-menu"><summary aria-label={t('taskActions')}>⋯</summary><div>{taskTransitions(task.status).filter((to) => to !== 'done').map((to) => <button key={to} type="button" onClick={() => write({ op: 'transitionTask', id: task.id, to })}>{to === 'planned' ? t('taskSchedule') : to === 'in_progress' ? t('taskStart') : to === 'dropped' ? t('taskDrop') : t('action.done')}</button>)}</div></details></li>)}</ul>{today.must_do.length === 0 && <EmptyState description={t('none')} action={null} />}</Card>
          <Card title={t('inbox')}><ul className="sin90-queue">{today.inbox.map((task) => <li key={task.id}>{task.title}</li>)}</ul>{today.inbox.length === 0 && <EmptyState description={t('none')} action={null} />}{pending.some((p) => p.ops.some((op) => opKind(op) === 'assign_task_direction')) && <div className="sin90-actions"><Badge tone="accent">{t('classified')}</Badge><Button variant="primary" disabled={!enabled} onClick={acceptAll}>{t('acceptAll')}</Button><Button onClick={() => { setLayer('today'); setProposalsOpen(true) }}>{t('reviewOneByOne')}</Button></div>}</Card>
        </section>
        <aside className="sin90-stack"><Card title={t('weekSummary')} actions={<Button size="sm" onClick={() => setLayer('rhythm')}>{t('seeWeek')}</Button>}>{week ? <><p>{t('thisWeek', { week: week.iso_week })}</p><p>{attentionText ?? t('attentionUnavailable')}</p></> : <p className="sin90-muted">{t('noWeek')}</p>}</Card><Card title={t('directions')}><ul className="sin90-queue">{directions.slice(0, 4).map((item) => <li key={item.id}>{item.title}<Badge tone="accent">{item.target_window === 'n/a' ? t('window.na') : item.target_window === 'ongoing' ? t('window.ongoing') : item.target_window}</Badge></li>)}</ul>{directions.length === 0 && <EmptyState description={t('noDirections')} action={null} />}</Card></aside>
      </div>
    </div>}

    {layer === 'direction' && groups && <div className="sin90-view"><div className="sin90-intro"><h2>{t('systemsTitle')}</h2></div>{areas.length === 0 && <Card title={t('systemsEmptyTitle')}><p>{t('systemsEmpty')}</p><Button variant="primary" disabled={!enabled} onClick={() => write({ op: 'installPack' })}>{t('installSystems')}</Button></Card>}<div className="sin90-area-grid">{groupDirections(areas, directions).map((group) => { const area = areas.find((item) => item.id === group.areaId); const draft = area ? areaDrafts[area.id] ?? { current: area.current_state ?? '', ideal: area.ideal_state ?? '' } : null; return <Card key={group.areaId ?? 'loose'} title={group.title ?? t('uncategorized')}>{area && <section className="sin90-area-state"><div className="sin90-area-state-lines"><p><span>{t('currentState')}</span> {area.current_state || '—'}</p><p><span>{t('idealState')}</span> {area.ideal_state || '—'}</p></div><Button size="sm" onClick={() => { setAreaDrafts((rows) => ({ ...rows, [area.id]: { current: area.current_state ?? '', ideal: area.ideal_state ?? '' } })); setAreaEditing((rows) => ({ ...rows, [area.id]: true })) }}>{t('edit')}</Button>{areaEditing[area.id] && draft && <div className="sin90-detail sin90-area-edit"><TextField id={`${area.id}-current`} label={t('currentState')} value={draft.current} onChange={(event) => setAreaDrafts((rows) => ({ ...rows, [area.id]: { ...draft, current: event.target.value } }))} /><TextField id={`${area.id}-ideal`} label={t('idealState')} value={draft.ideal} onChange={(event) => setAreaDrafts((rows) => ({ ...rows, [area.id]: { ...draft, ideal: event.target.value } }))} /><Button size="sm" disabled={!enabled} onClick={() => { setAreas((rows) => rows.map((row) => row.id === area.id ? { ...row, current_state: draft.current, ideal_state: draft.ideal } : row)); setAreaEditing((rows) => ({ ...rows, [area.id]: false })); write({ op: 'updateArea', id: area.id, current_state: draft.current || null, ideal_state: draft.ideal || null }) }}>{t('save')}</Button><Button size="sm" onClick={() => setAreaEditing((rows) => ({ ...rows, [area.id]: false }))}>{t('cancel')}</Button></div>}</section>}<ul className="sin90-queue">{group.directions.map((item) => { const plans = periodPlans.filter((plan) => plan.direction_id === item.id); const parents = legalPlanParents(planKind, planPeriod, plans, item.id); const yearNow = new Date().getFullYear(); const years = [yearNow, yearNow + 1, yearNow + 2].map((year) => plans.find((plan) => plan.kind === 'year' && Number(plan.period) === year)); const shown = plans.filter((plan) => plan.kind === 'year' ? Number(plan.period) >= yearNow && Number(plan.period) <= yearNow + 2 : plan.kind === 'quarter' ? plan.period === `${yearNow}-Q${Math.floor(new Date().getMonth() / 3) + 1}` : plan.kind === 'month' ? plan.period === `${yearNow}-${String(new Date().getMonth() + 1).padStart(2, '0')}` : plan.kind === 'half' && plan.period.startsWith(String(yearNow))); return <li className="sin90-direction-editor" key={item.id}><div className="sin90-direction-heading"><strong>{item.title}</strong><Badge>{item.target_window === 'n/a' ? t('window.na') : item.target_window === 'ongoing' ? t('window.ongoing') : item.target_window}</Badge></div><p className="sin90-three-year-goal"><strong>{t('threeYearGoal')}：</strong>{item.goal_3y || '—'}</p><div className="sin90-year-progress">{years.map((plan, index) => { const state = plan && progress.find((row) => row.plan_id === plan.id); return <div className="sin90-year-progress-item" key={index}><strong>{t(index === 0 ? 'thisYear' : index === 1 ? 'nextYear' : 'yearAfterNext')}</strong><span>{plan?.target || '—'}</span><div className="sin90-progress-track" role="progressbar" aria-label={`${t(index === 0 ? 'thisYear' : index === 1 ? 'nextYear' : 'yearAfterNext')} ${t('progress')}`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={state?.expected && state.actual !== null ? Math.max(0, Math.min(100, Math.round(state.actual / state.expected * 100))) : 0}><span style={{ width: `${state?.expected && state.actual !== null ? Math.max(0, Math.min(100, Math.round(state.actual / state.expected * 100))) : 0}%` }} /></div><small>{plan ? progressLabel(t, state?.status ?? 'on_track') : t('noPlan')}</small></div> })}</div><details><summary className="sin90-plan-edit-toggle">{t('editPlans')}</summary><div className="sin90-direction-content"><div className="sin90-detail"><TextField id={`${item.id}-goal-3y`} label={t('threeYearGoal')} value={item.goal_3y ?? ''} onChange={(event) => setDirections((rows) => rows.map((row) => row.id === item.id ? { ...row, goal_3y: event.target.value } : row))} /><TextField id={`${item.id}-goal-from-year`} label={t('goalFromYear')} inputMode="numeric" value={item.goal_from_year ?? ''} onChange={(event) => setDirections((rows) => rows.map((row) => row.id === item.id ? { ...row, goal_from_year: event.target.value ? Number(event.target.value) : null } : row))} /><Button size="sm" disabled={!enabled} onClick={() => write({ op: 'updateDirection', id: item.id, goal3y: item.goal_3y || null, goalFromYear: item.goal_from_year ?? null })}>{t('save')}</Button></div><ul className="sin90-queue">{shown.map((plan) => { const state = progress.find((row) => row.plan_id === plan.id); return <li key={plan.id}><span><strong>{t(`period.${plan.kind}`)} · {plan.period}</strong><TextField id={`${plan.id}-target-edit`} label={t('planTarget')} value={planEdits[plan.id]?.target ?? plan.target} onChange={(event) => setPlanEdits((rows) => ({ ...rows, [plan.id]: { target: event.target.value, metricTarget: rows[plan.id]?.metricTarget ?? String(plan.metric_target ?? '') } }))} /><TextField id={`${plan.id}-metric-edit`} label={t('metricTarget')} inputMode="numeric" value={planEdits[plan.id]?.metricTarget ?? String(plan.metric_target ?? '')} onChange={(event) => setPlanEdits((rows) => ({ ...rows, [plan.id]: { target: rows[plan.id]?.target ?? plan.target, metricTarget: event.target.value } }))} /></span><Badge tone={state?.status === 'at_risk' ? 'danger' : state?.status === 'behind' ? 'warning' : 'accent'}>{progressLabel(t, state?.status ?? 'on_track')}{state?.status === 'at_risk' ? ` · ${t('missWeeks', { weeks: state.miss_streak })}` : ''}</Badge><Button size="sm" disabled={!enabled} onClick={() => write({ op: 'updatePeriodPlan', id: plan.id, target: planEdits[plan.id]?.target ?? plan.target, metricTarget: planEdits[plan.id]?.metricTarget === '' ? null : Number(planEdits[plan.id]?.metricTarget ?? plan.metric_target) })}>{t('save')}</Button><Button size="sm" disabled={!enabled} onClick={() => write({ op: 'updatePeriodPlan', id: plan.id, status: 'dropped' })}>{t('drop')}</Button></li> })}</ul><div className="sin90-detail"><Select id={`${item.id}-plan-kind`} label={t('planLevel')} value={planKind} onChange={(event) => { const next = event.target.value as typeof planKind; const now = new Date(); const year = now.getFullYear(); setPlanKind(next); setPlanPeriod(next === 'year' ? String(year) : next === 'half' ? `${year}-H${now.getMonth() < 6 ? 1 : 2}` : next === 'quarter' ? `${year}-Q${Math.floor(now.getMonth() / 3) + 1}` : `${year}-${String(now.getMonth() + 1).padStart(2, '0')}`); setPlanParent('') }}>{(['year','half','quarter','month'] as const).map((kind) => <option key={kind} value={kind}>{t(`period.${kind}`)}</option>)}</Select><TextField id={`${item.id}-plan-period`} label={t('planPeriod')} value={planPeriod} onChange={(event) => setPlanPeriod(event.target.value)} /><Select id={`${item.id}-plan-parent`} label={t('planParent')} value={planParent} onChange={(event) => setPlanParent(event.target.value)}><option value="">{t('noParent')}</option>{parents.map((parent) => <option key={parent.id} value={parent.id}>{parent.period} · {parent.target}</option>)}</Select><TextField id={`${item.id}-new-plan-target`} label={t('planTarget')} value={planTarget} onChange={(event) => setPlanTarget(event.target.value)} /><Select id={`${item.id}-plan-metric`} label={t('planMetric')} value={planMetric} onChange={(event) => setPlanMetric(event.target.value as typeof planMetric)}><option value="none">{t('noNumber')}</option><option value="count">{t('count')}</option><option value="minutes">{t('minuteUnit')}</option></Select><TextField id={`${item.id}-plan-metric-target`} label={t('metricTarget')} inputMode="numeric" value={planMetricTarget} onChange={(event) => setPlanMetricTarget(event.target.value)} /><Button size="sm" disabled={!enabled || !planTarget.trim() || (planKind !== 'year' && !planParent)} onClick={() => write({ op: 'createPeriodPlan', directionId: item.id, kind: planKind, period: planPeriod, parentId: planParent || null, target: planTarget, metric: planMetric, metricTarget: planMetricTarget ? Number(planMetricTarget) : null })}>{t('addPlan')}</Button></div></div></details></li> })}</ul></Card> })}</div>{groups.length === 0 && areas.length > 0 && <EmptyState description={t('noDirections')} action={null} />}</div>}

    {layer === 'rhythm' && <div className="sin90-view"><div className={`sin90-period-columns${periodPlans.every((plan) => plan.kind !== 'half') ? ' sin90-period-no-half' : ''}`}>{(['year', 'half', 'quarter', 'month'] as const).map((kind) => <Card key={kind} title={t(`period.${kind}`)} className={kind === 'half' && periodPlans.every((plan) => plan.kind !== kind) ? 'sin90-half-empty' : undefined}>{periodPlans.filter((plan) => plan.kind === kind).map((plan) => { const status = progress.find((row) => row.plan_id === plan.id); return <article className="sin90-proposal" key={plan.id}><div className="sin90-rhythm-direction" title={directions.find((item) => item.id === plan.direction_id)?.title ?? ''}>{directions.find((item) => item.id === plan.direction_id)?.title ?? ''}</div><p className="sin90-rhythm-target">{plan.target}</p><small className="sin90-rhythm-meta">{plan.metric_target !== null ? `${plan.metric_target}${plan.metric === 'minutes' ? ` ${t('minuteUnit')}` : ` ${t('count')}`} · ` : ''}{progressLabel(t, status?.status ?? 'on_track')}</small>{status?.status === 'at_risk' && <p className="sin90-risk">{t('missWeeks', { weeks: status.miss_streak })}</p>}</article>})}{periodPlans.every((plan) => plan.kind !== kind) && <p className="sin90-muted">{t('none')}</p>}</Card>)}<Card title={t('weekColumn')} className="sin90-week-column" actions={week && weekNext(week.status) ? <Button size="sm" variant="primary" disabled={!enabled} onClick={() => write({ op: 'transitionWeek', id: week.id, to: weekNext(week.status) })}>{weekNext(week.status) === 'active' ? t('startWeek') : weekNext(week.status) === 'reviewing' ? t('reviewWeek') : t('closeWeek')}</Button> : !week ? <Button size="sm" variant="primary" disabled={!enabled} onClick={() => write({ op: 'createWeek', isoWeek: isoWeekUTC(new Date()) })}>{t('openWeek')}</Button> : undefined}>{renderWeek()}</Card></div></div>}

    {layer === 'review' && <div className="sin90-columns sin90-view"><section className="sin90-stack"><Card title={t('recentReviews')}>{reviews.length === 0 ? <EmptyState description={t('none')} action={null} /> : reviews.map((review) => <article className="sin90-proposal" key={review.id}><div className="sin90-row"><Badge>{reviewKindLabel(t, review.kind)}</Badge><span>{review.period}</span><Badge>{reviewLabel(t, review.status)}</Badge></div></article>)}{pending.map((proposal) => <article className="sin90-proposal" key={proposal.id}><div className="sin90-row"><Badge tone="accent">{sourceLabel(proposal.source)}</Badge>{proposal.ops.map((op) => impact(op, t)).map((text, index) => <Badge key={index}>{text}</Badge>)}</div>{proposal.ops.map((op, index) => <p key={index}>{describeOp(op, taskTitle, dirTitle)}</p>)}{proposal.rationale && <p className="sin90-muted">{proposal.rationale}</p>}<Button variant="primary" disabled={!enabled} onClick={() => write({ op: 'acceptProposal', id: proposal.id })}>{t('accept')}</Button><Button disabled={!enabled} onClick={() => write({ op: 'rejectProposal', id: proposal.id })}>{t('reject')}</Button></article>)}</Card></section><aside className="sin90-stack"><Card title={t('autoReview')}><div className="sin90-detail"><label><input type="checkbox" checked={reviewSettings.auto} onChange={(event) => setReviewSettings((value) => ({ ...value, auto: event.target.checked }))} /> {t('autoReview')}</label><Select label={t('frequency')} value={reviewSettings.cadence} onChange={(event) => setReviewSettings((value) => ({ ...value, cadence: event.target.value as Sin90ReviewSettings['cadence'] }))}><option value="weekly">{t('weekly')}</option><option value="monthly">{t('monthly')}</option></Select><Select label={t('weekday')} value={String(reviewSettings.weekday)} onChange={(event) => setReviewSettings((value) => ({ ...value, weekday: Number(event.target.value) }))}>{[1,2,3,4,5,6,7].map((day) => <option key={day} value={day}>{t(day === 1 ? 'weekday.1' : day === 2 ? 'weekday.2' : day === 3 ? 'weekday.3' : day === 4 ? 'weekday.4' : day === 5 ? 'weekday.5' : day === 6 ? 'weekday.6' : 'weekday.7')}</option>)}</Select><TextField label={t('hour')} inputMode="numeric" value={String(reviewSettings.hour)} onChange={(event) => setReviewSettings((value) => ({ ...value, hour: Number(event.target.value) }))} /><Button variant="primary" disabled={!enabled} onClick={() => write({ op: 'putReviewSettings', ...reviewSettings })}>{t('save')}</Button></div></Card><Card title={t('reviewImpact')}><p>{t('impact.today')} · {t('impact.week')} · {t('impact.rhythm')}</p></Card></aside></div>}
    {proposalsOpen && <div className="sin90-view"><Card title={t('proposals')} actions={<Button onClick={() => setProposalsOpen(false)}>{t('close')}</Button>}>{pending.map((proposal) => <article className="sin90-proposal" key={proposal.id}><Badge tone="accent">{sourceLabel(proposal.source)}</Badge>{proposal.ops.map((op, index) => <p key={index}><Badge>{impact(op, t)}</Badge> {describeOp(op, taskTitle, dirTitle)}</p>)}{proposal.rationale && <p className="sin90-muted">{proposal.rationale}</p>}<Button variant="primary" disabled={!enabled} onClick={() => write({ op: 'acceptProposal', id: proposal.id })}>{t('accept')}</Button><Button disabled={!enabled} onClick={() => write({ op: 'rejectProposal', id: proposal.id })}>{t('reject')}</Button></article>)}</Card></div>}
  </main>
}
