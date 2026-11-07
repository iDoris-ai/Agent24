// KB-C2-03: the area the KnowledgeView is drawn over, in the same place as
// Design. The page only reports where that area is and the theme; main owns
// the view, its address and its isolation (main/knowledge-view.ts). The
// sidebar entry that opens this page comes with #1625.

import { useEffect, useRef, useState } from 'react'
import type { KnowledgeViewResult, KnowledgeViewState, KnowledgeViewTheme } from '../../shared/ipc-types'
import { useT } from '../i18n'
import { knowledgeViewMessages } from './Knowledge.i18n'

function rectOf(el: HTMLElement): { x: number; y: number; width: number; height: number } {
  const rect = el.getBoundingClientRect()
  return { x: rect.x, y: rect.y, width: rect.width, height: rect.height }
}

/** The app shell marks light mode with `.app.light` (App.tsx). */
function currentTheme(el: HTMLElement | null): KnowledgeViewTheme {
  return el?.closest('.app')?.classList.contains('light') ? 'light' : 'dark'
}

type Shown = { kind: 'message'; key: string; reloadable: boolean } | null

function shownFor(result: KnowledgeViewResult | null, state: KnowledgeViewState | null): Shown {
  if (state) {
    if (state.state === 'loading') return { kind: 'message', key: 'loading', reloadable: false }
    if (state.state === 'degraded') return { kind: 'message', key: `degraded.${state.code}`, reloadable: state.code !== 'crashed' }
    if (state.state === 'closed') return { kind: 'message', key: `closed.${state.code}`, reloadable: true }
    if (state.state === 'unavailable') return { kind: 'message', key: `unavailable.${state.code}`, reloadable: false }
    if (state.state === 'ready') return null
  }
  if (result && !result.ok && result.code !== 'superseded') {
    if (result.code === 'crash_loop') return { kind: 'message', key: 'closed.crash_loop', reloadable: true }
    if (result.code === 'load_failed') return { kind: 'message', key: 'degraded.load_failed', reloadable: true }
    return { kind: 'message', key: `unavailable.${result.code}`, reloadable: false }
  }
  return null
}

export default function KnowledgePage(): JSX.Element {
  const t = useT(knowledgeViewMessages)
  const hostRef = useRef<HTMLDivElement>(null)
  const [result, setResult] = useState<KnowledgeViewResult | null>(null)
  const [state, setState] = useState<KnowledgeViewState | null>(null)

  useEffect(() => window.agent24.knowledgeView.onState((next) => setState(next)), [])

  useEffect(() => {
    const host = hostRef.current
    if (!host) return
    let cancelled = false
    const syncBounds = (): void => {
      if (!cancelled) void window.agent24.knowledgeView.bounds(rectOf(host))
    }
    const observer = new ResizeObserver(syncBounds)
    observer.observe(host)
    window.addEventListener('resize', syncBounds)
    void window.agent24.knowledgeView.show(rectOf(host), currentTheme(host)).then(
      (r) => { if (!cancelled) setResult(r) },
      () => { if (!cancelled) setResult({ ok: false, code: 'window_unavailable' }) },
    )
    return () => {
      cancelled = true
      observer.disconnect()
      window.removeEventListener('resize', syncBounds)
      void window.agent24.knowledgeView.hide()
    }
  }, [])

  const reload = (): void => {
    const host = hostRef.current
    if (!host) return
    setState(null)
    void window.agent24.knowledgeView.reload(rectOf(host), currentTheme(host)).then(setResult, () => setResult({ ok: false, code: 'window_unavailable' }))
  }

  const shown = shownFor(result, state)
  return (
    <div ref={hostRef} className="knowledge-host">
      {shown && (
        <div className="knowledge-message" role="status">
          <div>{t(shown.key as Parameters<typeof t>[0])}</div>
          {shown.reloadable && <button className="btn btn-primary" type="button" onClick={reload}>{t('action.reload')}</button>}
        </div>
      )}
    </div>
  )
}
