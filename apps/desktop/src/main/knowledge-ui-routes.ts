// KB-C2-04a (#1623): the host's copy of the UI path allowlist (UI-BRIDGE U4.5 ④,
// READONLY r11 R2.8; #1632 L1). The host injects the UI session only into a
// request whose method and normalised path match an entry here; the server's
// own list (WeKnora C2-01c, docs/agent24/ui-routes.json) is the source, and the
// copy may only be a subset of it (knowledge-ui-routes.test.ts compares them).
//
// An entry's path is literal segments, `:name` for exactly one non-empty
// segment, and at most one trailing `*name` for one or more segments (the wiki
// page read). Matching is exact, never by prefix: stricter than U4.5's prefix
// wording, so nothing the server did not list gets a session.
//
// #1670 c2 (UI-BRIDGE U1.3, READONLY R1.8 r13, OPS-v2 W1.6/I3): the three UI
// write ops and their five entries. POST is taken only for a write op, and a
// read op only with GET; `candidate.review` has two GET entries and one POST.

import copy from './knowledge-ui-routes.json'

/** The read-only display ops (READONLY R2.8, Weknora C2-01b2). */
export const UI_READ_OPS = ['kb.list', 'kb.get', 'doc.list', 'doc.get', 'chunk.list', 'wiki.list', 'wiki.get', 'tag.list'] as const
/** The three write ops a UI session may carry (READONLY R1.8 r13, UI-BRIDGE U1.3). */
export const UI_WRITE_OPS = ['candidate.review', 'candidate.approve', 'entry.withdraw'] as const
/** The closed set of UI ops a route may belong to. */
export const UI_OPS = [...UI_READ_OPS, ...UI_WRITE_OPS] as const
export type UiOp = (typeof UI_OPS)[number]
export type UiWriteOp = (typeof UI_WRITE_OPS)[number]

/** True when `op` is one of `UI_WRITE_OPS`. */
export function isWriteOp(op: UiOp): op is UiWriteOp {
  return (UI_WRITE_OPS as readonly string[]).includes(op)
}

export interface UiRoute {
  method: 'GET' | 'POST'
  path: string
  op: UiOp
}

const SEGMENT = /^(?:[A-Za-z0-9._~-]+|:[a-z][a-zA-Z0-9_]*|\*[a-z][a-zA-Z0-9_]*)$/

/**
 * Checks one list (the host copy or the server's) and returns its routes; throws on anything off.
 * POST is accepted only for a write op; `candidate.approve` and `entry.withdraw` only on POST.
 * `candidate.review` is accepted on GET and on POST. The dedup key stays `${method} ${path}`.
 */
export function parseUiRoutes(raw: unknown): UiRoute[] {
  const routes = (raw as { routes?: unknown } | null)?.routes
  if (!Array.isArray(routes)) throw new Error('ui routes: no routes array')
  const seen = new Set<string>()
  return routes.map((entry, i) => {
    const { method, path, op } = (entry ?? {}) as Record<string, unknown>
    if (method !== 'GET' && method !== 'POST') throw new Error(`ui routes[${i}]: GET or POST only`)
    if (typeof op !== 'string' || !(UI_OPS as readonly string[]).includes(op)) throw new Error(`ui routes[${i}]: op outside the closed set`)
    if (method === 'POST' && !isWriteOp(op as UiOp)) throw new Error(`ui routes[${i}]: POST only for a write op`)
    if (method === 'GET' && (op === 'candidate.approve' || op === 'entry.withdraw')) throw new Error(`ui routes[${i}]: ${op} only on POST`)
    if (typeof path !== 'string' || !path.startsWith('/') || path.endsWith('/')) throw new Error(`ui routes[${i}]: bad path`)
    const segments = path.slice(1).split('/')
    segments.forEach((s, j) => {
      if (!SEGMENT.test(s) || s === '.' || s === '..') throw new Error(`ui routes[${i}]: bad segment ${JSON.stringify(s)}`)
      if (s.startsWith('*') && j !== segments.length - 1) throw new Error(`ui routes[${i}]: *param only at the end`)
    })
    const key = `${method} ${path}`
    if (seen.has(key)) throw new Error(`ui routes[${i}]: duplicate ${key}`)
    seen.add(key)
    return { method, path, op: op as UiOp }
  })
}

function matches(route: UiRoute, segments: string[]): boolean {
  const pattern = route.path.slice(1).split('/')
  for (let i = 0; i < pattern.length; i++) {
    const p = pattern[i]
    if (p.startsWith('*')) return segments.length > i
    if (i >= segments.length) return false
    if (p.startsWith(':')) continue
    if (p !== segments[i]) return false
  }
  return segments.length === pattern.length
}

/**
 * The route a request may carry the UI session for, or null. `url` is the
 * request's full URL as the webRequest event gives it; the origin check (U4.5
 * ③) is the caller's. Anything that cannot be judged gives null.
 * GET and POST (exact case) match the route's method and its path; any other method is null.
 */
export function matchUiRoute(routes: readonly UiRoute[], method: string, url: string): UiRoute | null {
  if (method !== 'GET' && method !== 'POST') return null
  let parsed: URL
  try { parsed = new URL(url) } catch { return null }
  // U4.5 ④: an encoded slash or backslash in the raw path is refused outright.
  const rawPath = url.replace(/^[a-z][a-z0-9+.-]*:\/\/[^/?#]*/i, '').split(/[?#]/)[0]
  if (/%2f|%5c/i.test(rawPath)) return null
  const segments = parsed.pathname.slice(1).split('/')
  if (segments.some((s) => s === '')) return null
  return routes.find((r) => r.method === method && matches(r, segments)) ?? null
}

/** Host-copy entries the server's list does not have (must be empty). */
export function notOnServer(host: readonly UiRoute[], server: readonly UiRoute[]): UiRoute[] {
  const listed = new Set(server.map((r) => `${r.method} ${r.path} ${r.op}`))
  return host.filter((r) => !listed.has(`${r.method} ${r.path} ${r.op}`))
}

/** The host copy shipped with the app. */
export const KNOWLEDGE_UI_ROUTES: readonly UiRoute[] = parseUiRoutes(copy)
