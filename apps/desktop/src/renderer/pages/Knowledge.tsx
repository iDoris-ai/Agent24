// KB-C2-03: the area the KnowledgeView is drawn over, in the same place as
// Design. The page only reports where that area is and the theme; main owns
// the view, its address and its isolation (main/knowledge-view.ts). The
// sidebar entry that opens this page comes with #1625.

import { useContext, useEffect, useRef, useState } from 'react'
import type { KnowledgeViewCitation, KnowledgeViewConfirmAsk, KnowledgeViewResult, KnowledgeViewState, KnowledgeViewTheme } from '../../shared/ipc-types'
import { CapabilitiesContext } from '../capabilities/types'
import { useT } from '../i18n'
import { knowledgeViewMessages } from './Knowledge.i18n'
import { KnowledgeEntryPanel, usePendingEntry } from './chat/knowledge-entry'

function rectOf(el: HTMLElement): { x: number; y: number; width: number; height: number } {
  const rect = el.getBoundingClientRect()
  return { x: rect.x, y: rect.y, width: rect.width, height: rect.height }
}

/** The app shell marks light mode with `.app.light` (App.tsx). */
function currentTheme(el: HTMLElement | null): KnowledgeViewTheme {
  return el?.closest('.app')?.classList.contains('light') ? 'light' : 'dark'
}

type Shown = { kind: 'message'; key: string; reloadable: boolean } | null

const CLOSED_CODES = new Set<string>(['crash_loop', 'session_rejected', 'revocation_mismatch', 'probe_failed', 'empty_scope', 'bridge_closed'])

/** Not bound yet, or Off: the way out is the Knowledge binding in Settings (KB-C1). */
const SETTINGS_KEYS = new Set(['unavailable.knowledge_off', 'unavailable.not_configured', 'closed.bridge_closed_final'])

function shownFor(result: KnowledgeViewResult | null, state: KnowledgeViewState | null): Shown {
  if (state) {
    if (state.state === 'loading') return { kind: 'message', key: 'loading', reloadable: false }
    if (state.state === 'degraded') return { kind: 'message', key: `degraded.${state.code}`, reloadable: state.code !== 'crashed' }
    // U6.6: past the third bridge closing only Knowledge settings open the page again.
    if (state.state === 'closed' && state.final) return { kind: 'message', key: 'closed.bridge_closed_final', reloadable: false }
    if (state.state === 'closed') return { kind: 'message', key: `closed.${state.code}`, reloadable: true }
    if (state.state === 'unavailable') return { kind: 'message', key: `unavailable.${state.code}`, reloadable: false }
    if (state.state === 'ready') return null
  }
  if (result && !result.ok && result.code !== 'superseded') {
    if (CLOSED_CODES.has(result.code)) return { kind: 'message', key: `closed.${result.code}`, reloadable: true }
    if (result.code === 'load_failed' || result.code === 'session_unavailable') return { kind: 'message', key: `degraded.${result.code}`, reloadable: true }
    return { kind: 'message', key: `unavailable.${result.code}`, reloadable: false }
  }
  return null
}

export default function KnowledgePage(): JSX.Element {
  const t = useT(knowledgeViewMessages)
  const { open } = useContext(CapabilitiesContext)
  const hostRef = useRef<HTMLDivElement>(null)
  const [result, setResult] = useState<KnowledgeViewResult | null>(null)
  const [state, setState] = useState<KnowledgeViewState | null>(null)
  const [citation, setCitation] = useState<KnowledgeViewCitation>(null)
  const [confirmAsk, setConfirmAsk] = useState<Exclude<KnowledgeViewConfirmAsk, null> | null>(null)
  // KB-SOP-01a: a conversation the history list asked about ("沉淀为知识条目").
  const entry = usePendingEntry()

  useEffect(() => window.agent24.knowledgeView.onState((next) => setState(next)), [])
  useEffect(() => window.agent24.knowledgeView.onCitation((next) => setCitation(next)), [])
  useEffect(() => window.agent24.knowledgeView.onConfirm((ask) => setConfirmAsk(ask)), [])

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

  const acceptConfirm = (id: string): void => {
    void window.agent24.knowledgeView.answer(id, true)
    open('assistant', 'other')
    setConfirmAsk(null)
  }
  const declineConfirm = (id: string): void => {
    void window.agent24.knowledgeView.answer(id, false)
    setConfirmAsk(null)
  }

  const shown = shownFor(result, state)
  return (
    <div className="knowledge-page">
      <div ref={hostRef} className="knowledge-host">
        {shown && (
          <div className="knowledge-message" role="status">
            <div>{t(shown.key as Parameters<typeof t>[0])}</div>
            {shown.reloadable && <button className="btn btn-primary" type="button" onClick={reload}>{t('action.reload')}</button>}
            {SETTINGS_KEYS.has(shown.key) && (
              <button className="btn btn-primary" type="button" onClick={() => open('settings/connections', 'other')}>{t('action.openSettings')}</button>
            )}
          </div>
        )}
      </div>
      {entry && <KnowledgeEntryPanel id={entry} />}
      {!entry && confirmAsk && (
        <section className="knowledge-confirm" role="dialog" aria-modal="false">
          <p className="knowledge-confirm-count">{t('confirm.count', { count: confirmAsk.count })}</p>
          <ul className="knowledge-confirm-titles">
            {confirmAsk.titles.map((title, index) => (
              <li key={index}>{title ?? t('citation.untitled')}</li>
            ))}
          </ul>
          <button className="btn btn-primary" type="button" onClick={() => acceptConfirm(confirmAsk.id)}>{t('confirm.accept')}</button>
          <button className="btn" type="button" onClick={() => declineConfirm(confirmAsk.id)}>{t('confirm.decline')}</button>
        </section>
      )}
      {!entry && !confirmAsk && citation && (
        <aside className="knowledge-citation" aria-label={t('citation.title')}>
          <div className="knowledge-citation-title">{citation.title ?? t('citation.untitled')}</div>
          <div className="knowledge-citation-text">{citation.text}</div>
          <button className="btn" type="button" onClick={() => setCitation(null)}>{t('citation.dismiss')}</button>
        </aside>
      )}
    </div>
  )
}
