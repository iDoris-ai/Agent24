import { describe, expect, it } from 'vitest'
import { decideInjection, InjectedRequests, INJECTED_TRACK_LIMIT, type InjectRequest, type InjectTarget } from './knowledge-ui-inject'
import { parseUiRoutes } from './knowledge-ui-routes'

const ORIGIN = 'https://kb.example.org:8443'
const ROUTES = parseUiRoutes({
  routes: [
    { method: 'GET', path: '/api/v1/knowledge-bases', op: 'kb.list' },
    { method: 'GET', path: '/api/v1/knowledge-bases/:id', op: 'kb.get' },
  ],
})
const TOKEN = 'jwt-secret-value'
const PROOF = 'proof-secret-value'

function target(over: Partial<InjectTarget> = {}): InjectTarget {
  return { webContentsId: 7, origin: ORIGIN, routes: ROUTES, credentials: () => ({ token: TOKEN, proof: PROOF }), ...over }
}

function request(over: Partial<InjectRequest> = {}): InjectRequest {
  return {
    id: 1,
    webContentsId: 7,
    mainFrame: true,
    resourceType: 'xhr',
    method: 'GET',
    url: `${ORIGIN}/api/v1/knowledge-bases/kb-1`,
    requestHeaders: { Accept: 'application/json' },
    ...over,
  }
}

const injected = { cancel: false, injected: true, requestHeaders: { Accept: 'application/json', Authorization: `Bearer ${TOKEN}`, 'X-Agent24-View-Proof': PROOF } }
const plain = { cancel: false, injected: false, requestHeaders: { Accept: 'application/json' } }

describe('decideInjection (U4.5 ①–⑤)', () => {
  it('injects the session into an allowlisted fetch from the main frame', () => {
    expect(decideInjection(request(), target())).toEqual(injected)
    expect(decideInjection(request({ url: `${ORIGIN}/api/v1/knowledge-bases?page=2` }), target())).toEqual(injected)
  })

  it('strips a page-supplied Authorization or view proof, whatever the case, injected or not', () => {
    const forged = { Accept: 'application/json', authorization: 'Bearer forged', 'X-AGENT24-VIEW-PROOF': 'forged', AuThOrIzAtIoN: 'x' }
    expect(decideInjection(request({ requestHeaders: forged }), target())).toEqual(injected)
    expect(decideInjection(request({ requestHeaders: forged, url: `${ORIGIN}/other` }), target())).toEqual(plain)
  })

  it('① never injects for another webContents, another frame or no frame', () => {
    expect(decideInjection(request({ webContentsId: 8 }), target())).toEqual(plain)
    expect(decideInjection(request({ webContentsId: undefined }), target())).toEqual(plain)
    expect(decideInjection(request({ mainFrame: false }), target())).toEqual(plain)
    expect(decideInjection(request({ mainFrame: null }), target())).toEqual(plain)
  })

  it('② injects only into fetch/XHR, never a navigation, a WebSocket or a subresource', () => {
    for (const resourceType of ['mainFrame', 'subFrame', 'webSocket', 'script', 'image', 'other']) {
      expect(decideInjection(request({ resourceType }), target())).toEqual(plain)
    }
  })

  it('③ needs the exact allowed origin: scheme, host and port', () => {
    for (const url of [
      'http://kb.example.org:8443/api/v1/knowledge-bases/kb-1',
      'https://kb.example.org/api/v1/knowledge-bases/kb-1',
      'https://kb.example.org:8444/api/v1/knowledge-bases/kb-1',
      'https://evil.example.org:8443/api/v1/knowledge-bases/kb-1',
      'https://kb.example.org.evil.test:8443/api/v1/knowledge-bases/kb-1',
      'not a url',
    ]) {
      expect(decideInjection(request({ url }), target())).toEqual(plain)
    }
  })

  it('④ needs an allowlisted method and path; encoded separators are refused', () => {
    expect(decideInjection(request({ method: 'POST' }), target())).toEqual(plain)
    expect(decideInjection(request({ url: `${ORIGIN}/api/v1/models` }), target())).toEqual(plain)
    expect(decideInjection(request({ url: `${ORIGIN}/api/v1/knowledge-bases/kb-1%2fx` }), target())).toEqual(plain)
    expect(decideInjection(request({ url: `${ORIGIN}/api/v1/knowledge-bases/kb-1%5Cx` }), target())).toEqual(plain)
    expect(decideInjection(request(), target({ routes: [] }))).toEqual(plain)
  })

  it('⑤ cancels an eligible request while there is no session, rather than sending it bare', () => {
    expect(decideInjection(request(), target({ credentials: () => null }))).toEqual({ cancel: true })
  })

  it('does not even ask for the session when ①–④ fail', () => {
    let asked = 0
    const t = target({ credentials: () => { asked++; return { token: TOKEN, proof: PROOF } } })
    decideInjection(request({ webContentsId: 8 }), t)
    decideInjection(request({ url: `${ORIGIN}/api/v1/models` }), t)
    expect(asked).toBe(0)
  })

  it('an old instance\'s target never hands out the new JWT, nor the old one once it is gone', () => {
    let current: { token: string; proof: string } | null = { token: 'old-jwt', proof: 'old-proof' }
    const old = target({ credentials: () => current })
    current = null // the instance was destroyed
    expect(decideInjection(request(), old)).toEqual({ cancel: true })
    const fresh = target({ webContentsId: 9 })
    expect(decideInjection(request(), fresh)).toEqual(plain)
  })
})

describe('InjectedRequests (design §2.4)', () => {
  it('answers once per injected request id', () => {
    const t = new InjectedRequests()
    t.note(5)
    expect(t.take(6)).toBe(false)
    expect(t.take(5)).toBe(true)
    expect(t.take(5)).toBe(false)
  })

  it('keeps at most the limit and forgets the oldest', () => {
    const t = new InjectedRequests()
    for (let i = 0; i <= INJECTED_TRACK_LIMIT; i++) t.note(i)
    expect(t.take(0)).toBe(false)
    expect(t.take(1)).toBe(true)
    expect(t.take(INJECTED_TRACK_LIMIT)).toBe(true)
  })

  it('clear forgets everything', () => {
    const t = new InjectedRequests()
    t.note(1)
    t.clear()
    expect(t.take(1)).toBe(false)
  })
})
