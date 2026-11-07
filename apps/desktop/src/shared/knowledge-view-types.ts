// KB-C2-03: what the renderer and main exchange about the KnowledgeView. The
// renderer sends only bounds and a theme; it never sees the page address,
// partition or anything the page holds.

export interface KnowledgeViewBounds {
  x: number
  y: number
  width: number
  height: number
}

export type KnowledgeViewTheme = 'light' | 'dark'

/** Why the view is not showing; `crash_loop` is a `closed` code (CONTRACT-UI-BRIDGE-v1 §9.2). */
export type KnowledgeViewFailure =
  | 'window_unavailable'
  | 'knowledge_off'
  | 'not_configured'
  | 'partition_limit'
  | 'load_failed'
  | 'superseded'
  | 'crash_loop'

export type KnowledgeViewResult = { ok: true } | { ok: false; code: KnowledgeViewFailure }

export type KnowledgeViewState =
  | { state: 'hidden' }
  | { state: 'loading' }
  | { state: 'ready' }
  | { state: 'degraded'; code: 'crashed' | 'navigated_away' | 'load_failed' }
  | { state: 'closed'; code: 'crash_loop' }
  | { state: 'unavailable'; code: 'knowledge_off' | 'not_configured' | 'partition_limit' }
