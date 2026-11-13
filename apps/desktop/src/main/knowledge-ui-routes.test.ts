import { readFileSync } from 'node:fs'
import path from 'node:path'
import { describe, expect, it } from 'vitest'
import { isWriteOp, KNOWLEDGE_UI_ROUTES, matchUiRoute, notOnServer, parseUiRoutes, UI_OPS, type UiRoute } from './knowledge-ui-routes'

const ORIGIN = 'https://kb.example.org'
const ROUTES: UiRoute[] = parseUiRoutes({
  routes: [
    { method: 'GET', path: '/api/v1/knowledge-bases', op: 'kb.list' },
    { method: 'GET', path: '/api/v1/knowledge-bases/:id', op: 'kb.get' },
    { method: 'GET', path: '/api/v1/knowledge-bases/:id/wiki/*slug', op: 'wiki.get' },
    { method: 'POST', path: '/api/v1/candidates/:candidate_id/approve', op: 'candidate.approve' },
  ],
})

describe('the host copy against the server list (#1632 L1)', () => {
  // The pinned copy of WeKnora's docs/agent24/ui-routes.json; replaced verbatim
  // (with its commit in `source`) whenever the server list changes.
  const server = parseUiRoutes(JSON.parse(readFileSync(path.join(__dirname, '../../test/fixtures/weknora/ui-routes.json'), 'utf8')))

  it('lists nothing the server does not', () => {
    expect(notOnServer(KNOWLEDGE_UI_ROUTES, server)).toEqual([])
  })

  it('says which entries are missing on the server', () => {
    expect(notOnServer(ROUTES, ROUTES.slice(0, 2))).toEqual([ROUTES[2], ROUTES[3]])
    // The method is part of the entry too (#1670 c2).
    expect(notOnServer([{ ...ROUTES[3], method: 'GET' }], ROUTES)).toHaveLength(1)
    // The op is part of the entry: same path under another op is not on the list.
    expect(notOnServer([{ ...ROUTES[1], op: 'kb.list' }], ROUTES)).toHaveLength(1)
  })

  it('pins the server list at a WeKnora commit, and the host copy names the same one', () => {
    const fixture = path.join(__dirname, '../../test/fixtures/weknora/ui-routes.json')
    const pinned = JSON.parse(readFileSync(fixture, 'utf8')).source
    const host = JSON.parse(readFileSync(path.join(__dirname, 'knowledge-ui-routes.json'), 'utf8')).source
    expect(pinned).toMatchObject({ repo: 'iDoris-ai/WeKnora', path: 'docs/agent24/ui-routes.json' })
    expect(pinned.commit).toMatch(/^[0-9a-f]{40}$/)
    expect(host).toMatchObject({ repo: pinned.repo, path: pinned.path, commit: pinned.commit })
  })

  it('holds the eight read routes of C2-01c1, c2 and c3 (WeKnora#141, #142, #143)', () => {
    const ui = 'http://127.0.0.1:18282/api/v1/agent24/knowledge/ui'
    const o = `${ui}/knowledge-bases`
    const op = (url: string) => matchUiRoute(KNOWLEDGE_UI_ROUTES, 'GET', url)?.op ?? null
    expect(KNOWLEDGE_UI_ROUTES.filter((r) => r.method === 'GET' && !isWriteOp(r.op))).toHaveLength(8)
    expect(op(o)).toBe('kb.list')
    expect(op(`${o}/kb-1`)).toBe('kb.get')
    expect(op(`${o}/kb-1/tags`)).toBe('tag.list')
    expect(op(`${o}/kb-1/documents?page=2`)).toBe('doc.list')
    expect(op(`${ui}/documents/doc-1`)).toBe('doc.get')
    expect(op(`${ui}/documents/doc-1/chunks`)).toBe('chunk.list')
    expect(op(`${o}/kb-1/wiki/pages?page=2`)).toBe('wiki.list')
    expect(op(`${o}/kb-1/wiki/pages/overview`)).toBe('wiki.get')
    // A slug may hold slashes (WeKnora#143): the rest of the path is the slug.
    expect(op(`${o}/kb-1/wiki/pages/entity/acme-corp`)).toBe('wiki.get')
    // Not on the list: the wiki root, an empty slug, anything outside the UI prefix.
    expect(op(`${o}/kb-1/wiki`)).toBeNull()
    expect(op(`${o}/kb-1/wiki/pages/`)).toBeNull()
    expect(op(`${o}/kb-1/wiki/pages/entity%2Facme-corp`)).toBeNull()
    expect(op(`${ui}/documents`)).toBeNull()
    expect(op(`${ui}/documents/doc-1/chunks/c-1`)).toBeNull()
    expect(op('http://127.0.0.1:18282/api/v1/knowledge-bases')).toBeNull()
  })

  it('holds the five write entries of UI-BRIDGE U1.3, and POST for exactly three of them (#1670 c2)', () => {
    const ui = 'http://127.0.0.1:18282/api/v1/agent24/knowledge/ui'
    const op = (method: string, url: string) => matchUiRoute(KNOWLEDGE_UI_ROUTES, method, url)?.op ?? null
    expect(KNOWLEDGE_UI_ROUTES).toHaveLength(13)
    expect(op('GET', `${ui}/candidates?after=x`)).toBe('candidate.review')
    expect(op('GET', `${ui}/candidates/c-1`)).toBe('candidate.review')
    expect(op('POST', `${ui}/candidates/c-1/return`)).toBe('candidate.review')
    expect(op('POST', `${ui}/candidates/c-1/approve`)).toBe('candidate.approve')
    expect(op('POST', `${ui}/documents/k-1/withdraw`)).toBe('entry.withdraw')
    const prefix = '/api/v1/agent24/knowledge/ui'
    expect(KNOWLEDGE_UI_ROUTES.filter((r) => r.method === 'POST').map((r) => r.path)).toEqual([
      `${prefix}/candidates/:candidate_id/return`, `${prefix}/candidates/:candidate_id/approve`, `${prefix}/documents/:knowledge_id/withdraw`,
    ])
    // Every POST is a write op's; every read op is GET.
    expect(KNOWLEDGE_UI_ROUTES.filter((r) => r.method === 'POST').every((r) => isWriteOp(r.op))).toBe(true)
    expect(KNOWLEDGE_UI_ROUTES.filter((r) => !isWriteOp(r.op)).every((r) => r.method === 'GET')).toBe(true)
    // The wrong method on each entry gets nothing.
    expect(op('POST', `${ui}/candidates`)).toBeNull()
    expect(op('POST', `${ui}/candidates/c-1`)).toBeNull()
    expect(op('GET', `${ui}/candidates/c-1/approve`)).toBeNull()
    expect(op('GET', `${ui}/documents/k-1/withdraw`)).toBeNull()
    expect(op('POST', `${ui}/documents/k-1`)).toBeNull()
    // The Provider's write paths (OPS-v2 W8, C not the session) are never UI entries: propose with a session JWT is the server's credential_invalid.
    const provider = 'http://127.0.0.1:18282/api/v1/agent24/knowledge'
    for (const path of ['/candidates', '/candidates/withdraw', '/candidates/get']) expect(op('POST', `${provider}${path}`)).toBeNull()
    expect(op('POST', `${ui}/candidates/c-1/propose`)).toBeNull()
  })
})

describe('isWriteOp', () => {
  it('is true for the three write ops and nothing else', () => {
    expect(UI_OPS.filter(isWriteOp)).toEqual(['candidate.review', 'candidate.approve', 'entry.withdraw'])
  })
})

describe('parseUiRoutes', () => {
  it.each([
    [{ routes: [{ method: 'POST', path: '/a', op: 'kb.list' }] }, /POST only for a write op/],
    [{ routes: [{ method: 'PUT', path: '/a', op: 'candidate.approve' }] }, /GET or POST/],
    [{ routes: [{ method: 'post', path: '/a', op: 'candidate.approve' }] }, /GET or POST/],
    [{ routes: [{ method: ' GET', path: '/a', op: 'kb.list' }] }, /GET or POST/],
    [{ routes: [{ method: 'POST ', path: '/a', op: 'candidate.approve' }] }, /GET or POST/],
    [{ routes: [{ method: 'HEAD', path: '/a', op: 'kb.list' }] }, /GET or POST/],
    [{ routes: [{ method: 'GET', path: '/a', op: 'candidate.approve' }] }, /candidate.approve only on POST/],
    [{ routes: [{ method: 'GET', path: '/a', op: 'entry.withdraw' }] }, /entry.withdraw only on POST/],
    [{ routes: [{ method: 'GET', path: '/a', op: 'candidate.propose' }] }, /closed set/],
    [{ routes: [{ method: 'GET', path: '/a', op: 'search' }] }, /closed set/],
    [{ routes: [{ method: 'GET', path: 'a', op: 'kb.list' }] }, /bad path/],
    [{ routes: [{ method: 'GET', path: '/a/', op: 'kb.list' }] }, /bad path/],
    [{ routes: [{ method: 'GET', path: '/a//b', op: 'kb.list' }] }, /bad segment/],
    [{ routes: [{ method: 'GET', path: '/a/../b', op: 'kb.list' }] }, /bad segment/],
    [{ routes: [{ method: 'GET', path: '/a/*rest/b', op: 'kb.list' }] }, /only at the end/],
    [{ routes: [{ method: 'GET', path: '/a/%2f', op: 'kb.list' }] }, /bad segment/],
    [{ routes: [{ method: 'GET', path: '/a', op: 'kb.list' }, { method: 'GET', path: '/a', op: 'kb.get' }] }, /duplicate/],
    [{ routes: [{ method: 'POST', path: '/a', op: 'candidate.review' }, { method: 'POST', path: '/a', op: 'candidate.approve' }] }, /duplicate/],
    [{}, /no routes/],
    [null, /no routes/],
  ])('refuses %j', (raw, message) => {
    expect(() => parseUiRoutes(raw)).toThrow(message)
  })

  it('takes the same path once per method (#1670 c2)', () => {
    const routes = parseUiRoutes({ routes: [
      { method: 'GET', path: '/a', op: 'candidate.review' },
      { method: 'POST', path: '/a', op: 'candidate.review' },
    ] })
    expect(routes.map((r) => r.method)).toEqual(['GET', 'POST'])
  })
})

describe('matchUiRoute (U4.5 ④)', () => {
  const op = (method: string, url: string) => matchUiRoute(ROUTES, method, url)?.op ?? null

  it('matches exact paths, :param as one segment and *slug as the rest', () => {
    expect(op('GET', `${ORIGIN}/api/v1/knowledge-bases`)).toBe('kb.list')
    expect(op('GET', `${ORIGIN}/api/v1/knowledge-bases/kb1?page=2`)).toBe('kb.get')
    expect(op('GET', `${ORIGIN}/api/v1/knowledge-bases/kb1/wiki/a/b`)).toBe('wiki.get')
  })

  it('never matches by prefix or with a segment missing', () => {
    expect(op('GET', `${ORIGIN}/api/v1/knowledge-bases/kb1/documents`)).toBeNull()
    expect(op('GET', `${ORIGIN}/api/v1/knowledge-bases/kb1/wiki`)).toBeNull()
    expect(op('GET', `${ORIGIN}/api/v1`)).toBeNull()
    expect(op('GET', `${ORIGIN}/api/v1/knowledge-bases/`)).toBeNull()
  })

  it('takes GET and POST, exact case, each only on a route of its own method (#1670 c2)', () => {
    expect(op('POST', `${ORIGIN}/api/v1/knowledge-bases`)).toBeNull()
    expect(op('get', `${ORIGIN}/api/v1/knowledge-bases`)).toBeNull()
    expect(op('POST', `${ORIGIN}/api/v1/candidates/c-1/approve`)).toBe('candidate.approve')
    expect(op('GET', `${ORIGIN}/api/v1/candidates/c-1/approve`)).toBeNull()
    for (const method of ['post', 'PUT', 'DELETE', 'PATCH', 'OPTIONS', 'HEAD', 'POST ', ' POST']) expect(op(method, `${ORIGIN}/api/v1/candidates/c-1/approve`)).toBeNull()
    // A GET entry is not reached by HEAD, OPTIONS, a lower-case or a padded GET (Codex C3).
    for (const method of ['HEAD', 'OPTIONS', 'get', 'GET ', ' GET']) expect(op(method, `${ORIGIN}/api/v1/knowledge-bases`)).toBeNull()
  })

  it('picks the entry of the request\'s own method when GET and POST share a path (Codex C3)', () => {
    const shared = parseUiRoutes({ routes: [
      { method: 'GET', path: '/ui/candidates/:candidate_id', op: 'candidate.review' },
      { method: 'POST', path: '/ui/candidates/:candidate_id', op: 'candidate.approve' },
    ] })
    expect(matchUiRoute(shared, 'GET', `${ORIGIN}/ui/candidates/c-1`)).toEqual(shared[0])
    expect(matchUiRoute(shared, 'POST', `${ORIGIN}/ui/candidates/c-1`)).toEqual(shared[1])
    // Order does not decide it either.
    const reversed = [shared[1], shared[0]]
    expect(matchUiRoute(reversed, 'GET', `${ORIGIN}/ui/candidates/c-1`)).toEqual(shared[0])
  })

  it('refuses an encoded slash or backslash in the raw path, either case', () => {
    expect(op('GET', `${ORIGIN}/api/v1/knowledge-bases/kb1%2Fx`)).toBeNull()
    expect(op('GET', `${ORIGIN}/api/v1/knowledge-bases/kb1%5cx`)).toBeNull()
    // In the query it is not a path: still the kb.get route.
    expect(op('GET', `${ORIGIN}/api/v1/knowledge-bases/kb1?q=%2F`)).toBe('kb.get')
  })

  it('matches the normalised path, so dot segments cannot reach another route', () => {
    // /api/v1/knowledge-bases/kb1/../../x → /api/v1/x: not listed.
    expect(op('GET', `${ORIGIN}/api/v1/knowledge-bases/kb1/../../x`)).toBeNull()
    expect(op('GET', `${ORIGIN}/api/v1/x/../knowledge-bases`)).toBe('kb.list')
    expect(op('GET', `${ORIGIN}//api/v1/knowledge-bases`)).toBeNull()
  })

  it('gives null for what it cannot judge', () => {
    expect(op('GET', 'not a url')).toBeNull()
  })

})
