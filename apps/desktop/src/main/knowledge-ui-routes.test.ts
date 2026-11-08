import { readFileSync } from 'node:fs'
import path from 'node:path'
import { describe, expect, it } from 'vitest'
import { KNOWLEDGE_UI_ROUTES, matchUiRoute, notOnServer, parseUiRoutes, type UiRoute } from './knowledge-ui-routes'

const ORIGIN = 'https://kb.example.org'
const ROUTES: UiRoute[] = parseUiRoutes({
  routes: [
    { method: 'GET', path: '/api/v1/knowledge-bases', op: 'kb.list' },
    { method: 'GET', path: '/api/v1/knowledge-bases/:id', op: 'kb.get' },
    { method: 'GET', path: '/api/v1/knowledge-bases/:id/wiki/*slug', op: 'wiki.get' },
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
    expect(notOnServer(ROUTES, ROUTES.slice(0, 2))).toEqual([ROUTES[2]])
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

  it('holds the six UI routes of C2-01c1 and c2 (WeKnora#141, #142)', () => {
    const ui = 'http://127.0.0.1:18282/api/v1/agent24/knowledge/ui'
    const o = `${ui}/knowledge-bases`
    const op = (url: string) => matchUiRoute(KNOWLEDGE_UI_ROUTES, 'GET', url)?.op ?? null
    expect(KNOWLEDGE_UI_ROUTES).toHaveLength(6)
    expect(op(o)).toBe('kb.list')
    expect(op(`${o}/kb-1`)).toBe('kb.get')
    expect(op(`${o}/kb-1/tags`)).toBe('tag.list')
    expect(op(`${o}/kb-1/documents?page=2`)).toBe('doc.list')
    expect(op(`${ui}/documents/doc-1`)).toBe('doc.get')
    expect(op(`${ui}/documents/doc-1/chunks`)).toBe('chunk.list')
    // Not (yet) on the list: wiki comes with c3; nothing outside the UI prefix.
    expect(op(`${o}/kb-1/wiki`)).toBeNull()
    expect(op(`${ui}/documents`)).toBeNull()
    expect(op(`${ui}/documents/doc-1/chunks/c-1`)).toBeNull()
    expect(op('http://127.0.0.1:18282/api/v1/knowledge-bases')).toBeNull()
  })
})

describe('parseUiRoutes', () => {
  it.each([
    [{ routes: [{ method: 'POST', path: '/a', op: 'kb.list' }] }, /only GET/],
    [{ routes: [{ method: 'GET', path: '/a', op: 'search' }] }, /closed set/],
    [{ routes: [{ method: 'GET', path: 'a', op: 'kb.list' }] }, /bad path/],
    [{ routes: [{ method: 'GET', path: '/a/', op: 'kb.list' }] }, /bad path/],
    [{ routes: [{ method: 'GET', path: '/a//b', op: 'kb.list' }] }, /bad segment/],
    [{ routes: [{ method: 'GET', path: '/a/../b', op: 'kb.list' }] }, /bad segment/],
    [{ routes: [{ method: 'GET', path: '/a/*rest/b', op: 'kb.list' }] }, /only at the end/],
    [{ routes: [{ method: 'GET', path: '/a/%2f', op: 'kb.list' }] }, /bad segment/],
    [{ routes: [{ method: 'GET', path: '/a', op: 'kb.list' }, { method: 'GET', path: '/a', op: 'kb.get' }] }, /duplicate/],
    [{}, /no routes/],
    [null, /no routes/],
  ])('refuses %j', (raw, message) => {
    expect(() => parseUiRoutes(raw)).toThrow(message)
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

  it('takes GET only', () => {
    expect(op('POST', `${ORIGIN}/api/v1/knowledge-bases`)).toBeNull()
    expect(op('get', `${ORIGIN}/api/v1/knowledge-bases`)).toBeNull()
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
