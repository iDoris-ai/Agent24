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

/** `closed` codes (CONTRACT-UI-BRIDGE-v1 §9.2) this host can reach; `bridge_closed` comes with the bridge. */
export type KnowledgeViewClosedCode = 'crash_loop' | 'session_rejected' | 'revocation_mismatch' | 'probe_failed' | 'empty_scope'

/** Why the view is not showing; the `closed` codes are §9.2's. */
export type KnowledgeViewFailure =
  | 'window_unavailable'
  | 'knowledge_off'
  | 'not_configured'
  | 'partition_limit'
  | 'load_failed'
  | 'superseded'
  | 'session_unavailable'
  | KnowledgeViewClosedCode

export type KnowledgeViewResult = { ok: true } | { ok: false; code: KnowledgeViewFailure }

export type KnowledgeViewState =
  | { state: 'hidden' }
  | { state: 'loading' }
  | { state: 'ready' }
  | { state: 'degraded'; code: 'crashed' | 'navigated_away' | 'load_failed' | 'unresponsive' | 'session_unavailable' }
  | { state: 'closed'; code: KnowledgeViewClosedCode }
  | { state: 'unavailable'; code: 'knowledge_off' | 'not_configured' | 'partition_limit' }
