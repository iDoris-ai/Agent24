// KB-C2-04a (#1623): what the KnowledgeView partition's `onBeforeSendHeaders`
// does with one request (UI-BRIDGE U4.5; design §2.2). Kept free of Electron so
// every branch is a unit test; knowledge-view.ts turns the webRequest details
// into an InjectRequest and applies the decision.
//
// Every request loses whatever `Authorization` or `X-Agent24-View-Proof` it
// carried. The session goes on only when all of ①–⑤ hold:
//   ① the request comes from this instance's webContents and its main frame;
//   ② it is fetch/XHR (no navigation, no WebSocket, no subresource);
//   ③ its origin is exactly the allowed origin;
//   ④ method and path match the host's allowlist copy (no %2f / %5c);
//   ⑤ injection is not paused and the session has a JWT.
// ①–④ holding and only ⑤ failing cancels the request, so it never leaves
// without the session it needs. Anything that cannot be judged is not injected.
// Write entries (#1670 c2) go through the same ①–⑤ and never look at the
// session's `ops`: the server checks every write against the JWT and answers
// 403 operation_not_allowed (OPS-v2 W1.9 ③(a)). Evidence ruling 2026-11-11.

import { matchUiRoute, type UiRoute } from './knowledge-ui-routes'

const STRIPPED = new Set(['authorization', 'x-agent24-view-proof'])

export interface InjectRequest {
  id: number
  webContentsId: number | undefined
  /** true: the instance's main frame; false: another frame; null: no frame at all. */
  mainFrame: boolean | null
  resourceType: string
  method: string
  url: string
  requestHeaders: Record<string, string>
}

export interface InjectTarget {
  webContentsId: number
  origin: string
  routes: readonly UiRoute[]
  /** The JWT and view proof to send, or null while paused or not ready (⑤). Never logged. */
  credentials: () => { token: string; proof: string } | null
}

export type InjectDecision =
  | { cancel: true }
  | { cancel: false; requestHeaders: Record<string, string>; injected: boolean }

function sameOrigin(url: string, origin: string): boolean {
  try {
    return new URL(url).origin === origin
  } catch {
    return false
  }
}

export function decideInjection(request: InjectRequest, target: InjectTarget): InjectDecision {
  const headers: Record<string, string> = {}
  for (const [name, value] of Object.entries(request.requestHeaders)) {
    if (!STRIPPED.has(name.toLowerCase())) headers[name] = value
  }
  const eligible = request.webContentsId === target.webContentsId
    && request.mainFrame === true
    && request.resourceType === 'xhr'
    && sameOrigin(request.url, target.origin)
    && matchUiRoute(target.routes, request.method, request.url) !== null
  if (!eligible) return { cancel: false, requestHeaders: headers, injected: false }
  const credentials = target.credentials()
  if (!credentials) return { cancel: true }
  headers.Authorization = `Bearer ${credentials.token}`
  headers['X-Agent24-View-Proof'] = credentials.proof
  return { cancel: false, requestHeaders: headers, injected: true }
}

/** Upper bound on injected requests remembered at once; past it the oldest is forgotten. */
export const INJECTED_TRACK_LIMIT = 256

/**
 * The ids of requests that went out with the session (design §2.4): only a 401
 * on one of these starts the self-check (U3.8 looks at injected requests only).
 * An id is dropped when its response or error arrives.
 */
export class InjectedRequests {
  private readonly ids = new Set<number>()

  note(id: number): void {
    this.ids.add(id)
    if (this.ids.size > INJECTED_TRACK_LIMIT) {
      const oldest = this.ids.values().next().value as number
      this.ids.delete(oldest)
    }
  }

  /** True once for an injected request; forgets it either way. */
  take(id: number): boolean {
    return this.ids.delete(id)
  }

  clear(): void {
    this.ids.clear()
  }
}
