import { useEffect, useRef, useState } from 'react'
import { MachineFigure } from './home/MachineFigure'
import './home/home.css'
import { useT, type T } from '../i18n'
import { homeMessages } from './home/home.i18n'
import {
  STAGES, STATIONS, STATION_BY_ID, machineAt, stepTo, type MachineState, type StationId,
} from './home/machine-model'

type HomeKey = keyof typeof homeMessages.en

/** The readout caption for a machine state — the same words feed aria-valuetext. */
function captionOf(s: MachineState, t: T<HomeKey>): string {
  const source = t(s.stage === 'reply' ? `source.${s.source}.back` : `source.${s.source}`)
  return t(`stage.${s.stage}`, { source })
}

export interface HomePageProps {
  backendOk: boolean | null
  backendPort: number | null
  moduleCount: number
  onNavigate: (page: string) => void
}

function prefersReducedMotion(): boolean {
  return typeof window.matchMedia === 'function'
    && window.matchMedia('(prefers-reduced-motion: reduce)').matches
}

/**
 * Overview: the Agent24 machine. The clock only advances while the backend is
 * online — a dead daemon means a stopped machine, so the figure itself is a
 * health signal, not decoration.
 */
export default function HomePage({ backendOk, backendPort, moduleCount, onNavigate }: HomePageProps) {
  const [time, setTime] = useState(0)
  const [playing, setPlaying] = useState(() => !prefersReducedMotion())
  const [focus, setFocus] = useState<StationId | null>(null)
  const [picked, setPicked] = useState<StationId>('daemon')
  const status = backendOk === true ? 'online' : backendOk === false ? 'offline' : 'checking'
  const running = playing && status === 'online'
  const last = useRef<number | null>(null)
  const t = useT(homeMessages)

  // Turning on reduced motion while the page is open stops the machine too.
  useEffect(() => {
    if (typeof window.matchMedia !== 'function') return
    const mq = window.matchMedia('(prefers-reduced-motion: reduce)')
    const onChange = (e: MediaQueryListEvent) => { if (e.matches) setPlaying(false) }
    mq.addEventListener?.('change', onChange)
    return () => mq.removeEventListener?.('change', onChange)
  }, [])

  useEffect(() => {
    if (!running || typeof requestAnimationFrame !== 'function') return
    let id = 0
    const tick = (now: number) => {
      if (last.current !== null) {
        // Clamp so a backgrounded window doesn't fast-forward several runs.
        const dt = Math.min(now - last.current, 100)
        setTime((v) => v + dt)
      }
      last.current = now
      id = requestAnimationFrame(tick)
    }
    id = requestAnimationFrame(tick)
    return () => { cancelAnimationFrame(id); last.current = null }
  }, [running])

  const s = machineAt(time)
  const step = (delta: number) => { setPlaying(false); setTime((v) => stepTo(v, delta)) }
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowRight') step(1)
    else if (e.key === 'ArrowLeft') step(-1)
    else if (e.key === 'Home') { setPlaying(false); setTime((v) => stepTo(v, -machineAt(v).stageIndex)) }
    else if (e.key === 'End') { setPlaying(false); setTime((v) => stepTo(v, STAGES.length - 1 - machineAt(v).stageIndex)) }
    else if (e.key === ' ') setPlaying((p) => !p)
    else return
    e.preventDefault()
  }

  const shown = STATION_BY_ID[focus ?? picked]
  const caption = status === 'online'
    ? captionOf(s, t)
    : status === 'offline' ? t('readout.offline') : t('readout.checking')

  return (
    <div className="content home-page">
      <div className="page-title">{t('page.title')}</div>
      <div className="page-sub">{t('page.sub')}</div>

      <div className="home-machine">
        <div
          className="home-figure"
          tabIndex={0}
          role="slider"
          aria-label={t('figure.label')}
          aria-valuemin={1}
          aria-valuemax={STAGES.length}
          aria-valuenow={s.stageIndex + 1}
          aria-valuetext={status === 'online' ? t('readout.aria', { step: s.stageIndex + 1, total: STAGES.length, caption }) : caption}
          onKeyDown={onKey}
        >
          <MachineFigure state={s} status={status} focus={focus} onFocus={setFocus} onPick={setPicked} label={t('figure.svgLabel')} />
        </div>

        <aside className="home-panel">
          <div className={`home-readout is-${status}`}>
            <div className="home-readout-head">
              <span className="home-step">{status === 'online' ? `${s.stageIndex + 1} / ${STAGES.length}` : '— / —'}</span>
              <span className="home-run">{t('readout.run', { n: s.runNo + 1 })}</span>
            </div>
            {/* Announce only steps the user asked for — an autoplaying loop would
                otherwise queue a screen-reader announcement every couple of seconds. */}
            <div className="home-caption" aria-live={playing && status === 'online' ? 'off' : 'polite'}>{caption}</div>
            <div className="home-progress" aria-hidden="true">
              {STAGES.map((st, i) => (
                <span key={st.id} className={i < s.stageIndex ? 'done' : i === s.stageIndex ? 'now' : ''} />
              ))}
            </div>
            <div className="home-controls">
              <button onClick={() => step(-1)} aria-label={t('ctl.prev')}>◀</button>
              <button onClick={() => setPlaying((p) => !p)} disabled={status !== 'online'}>
                {playing ? t('ctl.pause') : t('ctl.play')}
              </button>
              <button onClick={() => step(1)} aria-label={t('ctl.next')}>▶</button>
            </div>
          </div>

          <dl className="home-live">
            <div><dt>{t('live.daemon')}</dt><dd>{status === 'online' ? `${t('live.running')}${backendPort !== null ? ` :${backendPort}` : ''}` : status === 'offline' ? t('live.offline') : t('live.checking')}</dd></div>
            <div><dt>{t('live.modules')}</dt><dd>{moduleCount}</dd></div>
            <div><dt>{t('live.ledger')}</dt><dd>{t('live.illustrative', { n: s.ledger })}</dd></div>
          </dl>

          <ol className="home-legend">
            {STATIONS.map((st) => (
              <li key={st.id}>
                <button
                  className={`${status === 'online' && s.lit.includes(st.id) ? 'is-lit' : ''}${shown.id === st.id ? ' is-focus' : ''}`}
                  onMouseEnter={() => setFocus(st.id)}
                  onMouseLeave={() => setFocus(null)}
                  onClick={() => setPicked(st.id)}
                >
                  <span className="home-legend-n">{st.n}</span>
                  <span className="home-legend-name">{t(`st.${st.id}.name`)}</span>
                  <span className="home-legend-sub">{t(`st.${st.id}.sub`)}</span>
                </button>
              </li>
            ))}
          </ol>

          <div className="home-detail">
            <div className="home-detail-name">{shown.n} · {t(`st.${shown.id}.name`)}</div>
            <p>{t(`st.${shown.id}.detail`)}</p>
            {shown.page && (
              <button className="home-open" onClick={() => onNavigate(shown.page!)}>{t('detail.open')}</button>
            )}
          </div>
        </aside>
      </div>
    </div>
  )
}
