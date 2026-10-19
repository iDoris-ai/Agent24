// KB-CT-02b (#1012): the Knowledge v1 fixtures (protocol/knowledge/fixtures/v1)
// must match the digests pinned in their manifest and the schemas in
// protocol/knowledge/v1 (docs/knowledge/CONTRACT-READONLY-v1.md r8). The Rust
// side (agent24-protocol tests/knowledge_fixtures.rs) checks the same manifest,
// so changing any fixture byte turns both red. Pure schema test, no daemon.

import { beforeAll, describe, expect, it } from 'vitest'
import { createHash } from 'node:crypto'
import { readFileSync, readdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import Ajv2020 from 'ajv/dist/2020.js'

type Validator = ((doc: unknown) => boolean) & { errors?: unknown }
type Entry = { file: string; version: number; class: 'accept' | 'reject'; request_valid: boolean | null; server_handler: boolean; sha256: string }

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')
const schemaDir = join(repoRoot, 'protocol', 'knowledge', 'v1')
const fixtureDir = join(repoRoot, 'protocol', 'knowledge', 'fixtures', 'v1')
const BASE = 'https://github.com/iDoris-ai/Agent24/protocol/knowledge/v1/'
const manifest = JSON.parse(readFileSync(join(fixtureDir, 'manifest.json'), 'utf8')) as {
  contract_revision: string; protocol_version: number; fixtures: Entry[]
}
// Pins the whole fixture set (every file digest and version). Changing a fixture
// means bumping its version, re-pinning its sha256 and updating this constant
// together with MANIFEST_SHA256 in rust/crates/agent24-protocol/tests/knowledge_fixtures.rs.
const MANIFEST_SHA256 = 'e72773edb4d6a5f09d52f1f2c2b0efc0591c97fe629d6294d6d3d4a4484b94df'
// Cases the contract names (#1012; §11.3, §11.7; §12 #11, #18, #19; r6, r8); same list as the Rust test.
const REQUIRED = [
  'search.empty-scope.json', 'search.scope-empty-array.json', 'search.partial.json', 'search.partial-overloaded.json',
  'search.cancelled.json', 'search.timeout.json', 'search.unauthorized-revoked.json', 'read.out-of-scope.json',
  'read.stale-chunk-edited.json', 'read.stale-anchor-missing.json', 'read.stale-document.json',
  'read.stale-document-building.json', 'ready.scoped.json', 'ready.dependency.json', 'capabilities.integration.json',
  'search.invalid-body.json', 'search.unauthorized-missing.json', 'search.operation-not-allowed.json',
  'search.operation-disabled.json', 'search.unauthorized-before-parse.json',
  'read.policy-denied.json', 'read.timeout.json', 'read.index-failed.json', 'read.expected-revision-partial.json',
  'read.stale-page.json', 'ready.operation-disabled.json', 'read.operation-disabled.json',
]
const load = (file: string): any => JSON.parse(readFileSync(join(fixtureDir, file), 'utf8'))
const clone = <T>(x: T): T => JSON.parse(JSON.stringify(x))
// Contract §6 table: suggested HTTP status per wire code.
const STATUS: Record<string, number> = {
  invalid_request: 400, unauthorized: 401, forbidden: 403, not_found: 404, stale: 409, timeout: 504, unavailable: 503, internal: 500,
}
const SCHEMA: Record<string, string> = { 'knowledge.search': 'search', 'knowledge.read': 'read', 'knowledge.ready': 'ready' }
const v: Record<string, Validator> = {}

beforeAll(() => {
  const ajv = new Ajv2020.default({ strictSchema: true, strictTypes: false, allErrors: true })
  for (const f of readdirSync(schemaDir)) ajv.addSchema(JSON.parse(readFileSync(join(schemaDir, f), 'utf8')))
  const compile = (ref: string) => ajv.compile({ $ref: BASE + ref }) as Validator
  for (const s of Object.values(SCHEMA)) {
    v[`${s}.request`] = compile(`${s}.schema.json#/$defs/request`)
    v[`${s}.response`] = compile(`${s}.schema.json#/$defs/response`)
  }
  v.error = compile('error.schema.json')
  v.cancelled = compile('local-cancelled.schema.json')
  v.capabilities = compile('capabilities.schema.json')
})

const ok = (name: string, doc: unknown, want = true) => expect(v[name](doc), `${name}: ${JSON.stringify(v[name].errors)}`).toBe(want)
const responseSchema = (op: string) => (op === 'system.capabilities' ? 'capabilities' : `${SCHEMA[op]}.response`)

describe('knowledge v1 fixtures — manifest', () => {
  it('pins exactly the files in the directory, by version and sha256', () => {
    expect(manifest.contract_revision).toBe('r8')
    // The manifest pins the revision named in the contract's own header.
    const contract = readFileSync(join(repoRoot, 'docs', 'knowledge', 'CONTRACT-READONLY-v1.md'), 'utf8')
    expect(contract.match(/文档修订 (r\d+)/)?.[1]).toBe(manifest.contract_revision)
    expect(manifest.protocol_version).toBe(1)
    const pinned = createHash('sha256').update(readFileSync(join(fixtureDir, 'manifest.json'))).digest('hex')
    expect(pinned, '改夹具要升 version，并更新清单摘要常量 (MANIFEST_SHA256, TS and Rust)').toBe(MANIFEST_SHA256)
    for (const f of REQUIRED) expect(manifest.fixtures.map((e) => e.file), `missing ${f}`).toContain(f)
    const onDisk = readdirSync(fixtureDir).filter((f) => f !== 'manifest.json').sort()
    expect(manifest.fixtures.map((e) => e.file).sort()).toEqual(onDisk)
    for (const e of manifest.fixtures) {
      expect(Number.isInteger(e.version) && e.version >= 1, e.file).toBe(true)
      const actual = createHash('sha256').update(readFileSync(join(fixtureDir, e.file))).digest('hex')
      expect(actual, `${e.file}: digest changed; bump its version and pin the new sha256`).toBe(e.sha256)
    }
  })

  it('files partial as a success (§12 #11) and keeps only the cancel fixture out of server tests (§12 #18)', () => {
    const by = Object.fromEntries(manifest.fixtures.map((e) => [e.file, e]))
    expect(by['search.partial.json'].class).toBe('accept')
    expect(manifest.fixtures.filter((e) => !e.server_handler).map((e) => e.file)).toEqual(['search.cancelled.json'])
  })
})

describe('knowledge v1 fixtures — schemas and contract rules', () => {
  it.each(manifest.fixtures.map((e) => [e.file, e] as const))('%s', (file, e) => {
    const fx = load(file)
    const { status, body } = fx.response
    // Manifest flags agree with the file (same checks as the Rust test).
    expect(['accept', 'reject']).toContain(e.class)
    expect(fx.request === undefined, file).toBe(e.request_valid === null)
    if (e.request_valid === false) expect(e.class).toBe('reject')
    expect('local' in fx, file).toBe(!e.server_handler)
    if (e.request_valid !== null) ok(`${SCHEMA[fx.op]}.request`, fx.request, e.request_valid)
    if (e.class === 'accept') {
      expect(status).toBe(200)
      ok(responseSchema(fx.op), body)
    } else {
      ok('error', body)
      expect(status).toBe(STATUS[body.error.code])
    }
    if (typeof fx.request === 'string') {
      // R6.1: a raw request stands for a body that does not parse, so nothing is echoed.
      expect(() => JSON.parse(fx.request)).toThrow()
      expect([body.error.details.request_id, body.error.details.generation]).toEqual([null, null])
    } else if (fx.request) {
      // R7.2: request_id and generation are echoed, except where R6.1 lets the error echo null.
      const echo = e.class === 'accept' ? body : body.error.details
      for (const k of ['request_id', 'generation']) {
        if (echo[k] === null) expect(e.class === 'reject' && mayEchoNull(body.error), `${file}: null ${k}`).toBe(true)
        else expect(echo[k], k).toBe(fx.request[k])
      }
    }
    if (e.class === 'reject' && typeof fx.request === 'object') rejectPairsWithRequest(fx.request, body.error)
    if (e.class !== 'accept' || !fx.request) return
    const asked: string[] = fx.request.scope?.knowledge_base_ids ?? []
    const inE = (kb: string) => body.effective_scope.knowledge_base_ids.includes(kb)
    if (body.effective_scope) expect(body.effective_scope.knowledge_base_ids.every((kb: string) => asked.includes(kb))).toBe(true)
    if (fx.op === 'knowledge.search') {
      // R3.6: authoritative order, rank 1..n, one score_space per response; R3.3: clamped budget.
      expect(body.hits.map((h: any) => h.rank)).toEqual(body.hits.map((_: unknown, i: number) => i + 1))
      expect(body.hits.length).toBeLessThanOrEqual(body.applied_budget.max_results)
      for (const [k, applied] of Object.entries(body.applied_budget)) {
        if (fx.request.budget?.[k] !== undefined) expect(applied, k).toBeLessThanOrEqual(fx.request.budget[k])
      }
      expect(new Set(body.hits.map((h: any) => h.score_space)).size).toBeLessThanOrEqual(1)
      for (const h of body.hits) {
        expect(inE(h.knowledge_base_id)).toBe(true)
        if (h.locator.valid) expect(h.locator.end).toBeGreaterThan(h.locator.start)
      }
      for (const pf of body.partial_failures ?? []) expect(inE(pf.knowledge_base_id)).toBe(true)
      // R3.4/R3.5: one entry per failed KB, no hit from a failed KB, and partial
      // means at least one branch in E succeeded.
      const failedIds: string[] = (body.partial_failures ?? []).map((pf: any) => pf.knowledge_base_id)
      const failed = new Set(failedIds)
      expect(failed.size).toBe(failedIds.length)
      for (const h of body.hits) expect(failed.has(h.knowledge_base_id), h.knowledge_base_id).toBe(false)
      if (body.completeness === 'partial') {
        expect(body.effective_scope.knowledge_base_ids.some((kb: string) => !failed.has(kb))).toBe(true)
      }
    }
    if (fx.op === 'knowledge.read' && fx.request.chunk_id) {
      expect(body.segments.map((s: any) => s.chunk_id)).toEqual([fx.request.chunk_id])
    }
    const want = fx.op === 'knowledge.read' ? fx.request.expected_revision : undefined
    if (want) {
      // R4.4: a successful read is at the expected revision.
      expect([body.revision.source_revision, body.revision.index_build_id]).toEqual([want.source_revision, want.index_build_id])
      if (fx.request.chunk_id) expect(body.segments[0].chunk_revision).toBe(want.chunk_revision)
    }
    if (fx.op === 'knowledge.ready') {
      // R8.2/R8.4: per-KB entries only with scope, and only for KBs that were asked for.
      expect(body.knowledge_bases !== undefined).toBe(fx.request.scope !== undefined)
      const kbIds: string[] = (body.knowledge_bases ?? []).map((kb: any) => kb.knowledge_base_id)
      for (const kb of kbIds) expect(asked).toContain(kb)
      expect(new Set(kbIds).size).toBe(kbIds.length)
    }
  })
})

// Rules that tie an error to the request it answers (§6, R3.6, R4.4); the Rust
// test (knowledge_schema.rs) checks the same.
function rejectPairsWithRequest(request: any, error: any) {
  const d = error.details
  if (error.code === 'stale') {
    const want = request.expected_revision
    const sameBuild = want && d.current_revision.source_revision === want.source_revision &&
      d.current_revision.index_build_id === want.index_build_id
    if (!request.chunk_id) {
      // Document level: no reason, no chunk revision, and the pair really moved.
      expect([d.reason, d.current_chunk_revision]).toEqual([undefined, undefined])
      if (want) expect(sameBuild).toBe(false)
    } else if (d.reason !== undefined) {
      expect(d.reason).toBe('anchor_missing')
    } else {
      // Chunk edited in the same build: the new chunk revision is reported.
      expect(sameBuild).toBe(true)
      expect(d.current_chunk_revision).toBeDefined()
      expect(d.current_chunk_revision).not.toBe(want.chunk_revision)
    }
  }
  if (d.reason === 'mixed_score_space') {
    const spaces = d.groups.map((g: any) => g.score_space)
    const kbs = d.groups.flatMap((g: any) => g.knowledge_base_ids)
    expect(new Set(spaces).size).toBe(spaces.length)
    expect(new Set(kbs).size).toBe(kbs.length)
    for (const kb of kbs) expect(request.scope.knowledge_base_ids).toContain(kb)
  }
}

// R6.1, R6.4: the errors that may echo null (unauthorized, internal, and an
// invalid_request other than mixed_score_space, which comes after E).
function mayEchoNull(error: any) {
  return ['unauthorized', 'internal'].includes(error.code) ||
    (error.code === 'invalid_request' && error.details.reason !== 'mixed_score_space')
}

// R7.3: the client accepts a response only if it answers an in-flight request
// sent at the current generation; otherwise it drops it and returns local
// cancelled. A response is paired by its echo; an error whose echo R6.1 lets
// be null is paired by the HTTP exchange that carried it (`sent`).
// This is a specification example, not product code: once the Provider lands
// (KB-C1-02a) this test must exercise the Provider instead.
function receive(body: any, sent: { request_id: string; generation: number }, inFlight: Set<string>, currentGeneration: number) {
  const echo = body.error ? body.error.details : body
  const id = echo.request_id === null || echo.generation === null ? sent : echo
  if (inFlight.has(id.request_id) && id.generation === currentGeneration) return body
  return {
    error: {
      code: 'cancelled', message: 'Request was cancelled.',
      details: { retryable: false, request_id: id.request_id, generation: id.generation, current_generation: currentGeneration },
    },
  }
}

describe('knowledge v1 — cancellation (§11.4, §12 #18)', () => {
  it('drops the late response and yields the pinned local result, which never validates as a wire error', () => {
    const fx = load('search.cancelled.json')
    ok('search.response', fx.response.body)
    ok('cancelled', fx.local.result)
    ok('error', fx.local.result, false)
    const inFlight = new Set([fx.request.request_id])
    expect(receive(fx.response.body, fx.request, inFlight, fx.request.generation)).toBe(fx.response.body)
    expect(receive(fx.response.body, fx.request, inFlight, fx.local.current_generation)).toEqual(fx.local.result)
    // The other drop path: the generation is current but the request_id is not in flight.
    const stray = receive(fx.response.body, fx.request, new Set(['01JB0000000000000000000000']), fx.request.generation)
    expect(stray.error.code).toBe('cancelled')
    expect(stray.error.details.current_generation).toBe(fx.request.generation)
    ok('cancelled', stray)
  })

})

describe('knowledge v1 schemas — canonical negatives', () => {
  it('search: partial needs partial_failures, complete has none; null chunk_revision means unknown', () => {
    const partial = load('search.partial.json').response.body
    ok('search.response', { ...partial, partial_failures: [] }, false)
    ok('search.response', { ...partial, completeness: 'complete' }, false)
    const res = clone(load('search.ok.json').response.body)
    res.hits[2].index_state = 'fresh'
    ok('search.response', res, false)
  })

  it('read: expected_revision has three parts with chunk_id, two without', () => {
    const chunk = load('read.chunk.json').request
    ok('read.request', { ...chunk, expected_revision: { source_revision: 's', index_build_id: 'i' } }, false)
    ok('read.request', { ...chunk, expected_revision: { ...chunk.expected_revision, chunk_revision: null } }, false)
    const doc = load('read.document.json').request
    ok('read.request', { ...doc, expected_revision: { ...doc.expected_revision, chunk_revision: 0 } }, false)
    // R4.1: page.limit is optional; continuing a single-chunk read needs only the cursor.
    ok('read.request', { ...chunk, page: { cursor: 'cur-71-1' } })
    ok('read.request', { ...chunk, page: { limit: 5 } }, false)
  })

  it('errors: retryable and details are fixed per code; cancelled is not a wire code', () => {
    const mutate = (file: string, edit: (d: any) => void) => {
      const body = clone(load(file).response.body)
      edit(body.error)
      return body
    }
    ok('error', mutate('read.out-of-scope.json', (e) => { e.details.reason = 'out_of_scope' }), false)
    ok('error', mutate('read.stale-anchor-missing.json', (e) => { e.details.current_chunk_revision = 3 }), false)
    ok('error', mutate('search.unavailable-mixed.json', (e) => { e.details.retryable = true }), false)
    // R3.4: a non-retryable partial failure makes the error non-retryable, with or without a reason.
    ok('error', mutate('search.unavailable-mixed.json', (e) => { e.details.reason = 'dependency'; e.details.retryable = true }), false)
    ok('error', mutate('search.unavailable-mixed.json', (e) => { delete e.details.reason; e.details.retryable = true }), false)
    // §6 (r8): retryable false requires the non-retryable reason.
    ok('error', mutate('search.unavailable-mixed.json', (e) => { delete e.details.reason }), false)
    ok('error', mutate('search.unavailable-mixed.json', (e) => { delete e.details.partial_failures; e.details.reason = 'dependency' }), false)
    // R6.1: null echoes only for invalid_request, unauthorized and internal.
    ok('error', mutate('read.out-of-scope.json', (e) => { e.details.request_id = null }), false)
    ok('error', mutate('read.internal.json', (e) => { e.details.request_id = null; e.details.generation = null }))
    // An invalid_request with a reason was parsed, so it echoes (R6.4).
    ok('error', mutate('search.mixed-score-space.json', (e) => { e.details.request_id = null; e.details.generation = null }), false)
    ok('error', mutate('read.stale-chunk-edited.json', (e) => { e.details.current_revision.index_build_id = null }), false)
    ok('error', mutate('search.empty-scope.json', (e) => { e.code = 'cancelled' }), false)
    ok('error', mutate('search.mixed-score-space.json', (e) => { delete e.details.groups }), false)
    // §6 (r8): unauthorized and forbidden must carry a reason; timeout pairs only with timeout (R3.5).
    ok('error', mutate('search.unauthorized-revoked.json', (e) => { delete e.details.reason }), false)
    ok('error', mutate('search.empty-scope.json', (e) => { delete e.details.reason }), false)
    ok('error', mutate('search.unavailable-mixed.json', (e) => { e.details.partial_failures[0].reason = 'overloaded' }), false)
    // §6, R3.4: the non-retryable top-level reason must appear among the listed failures ...
    ok('error', mutate('search.unavailable-mixed.json', (e) => { e.details.partial_failures[1].reason = 'unsupported_kb_type' }), false)
    ok('error', mutate('search.unavailable-mixed.json', (e) => {
      e.details.reason = 'unsupported_kb_type'
    }), false)
    // ... a non-retryable error with listed failures lists a non-retryable one ...
    ok('error', mutate('search.unavailable-mixed.json', (e) => {
      e.details.partial_failures[1] = { knowledge_base_id: 'kb-c', code: 'unavailable', reason: 'dependency', retryable: true }
    }), false)
    // ... and index_pending (document-level read) never carries partial_failures.
    ok('error', mutate('read.index-pending.json', (e) => {
      e.details.partial_failures = [{ knowledge_base_id: 'kb-c', code: 'unavailable', reason: 'dependency', retryable: true }]
    }), false)
  })

  it('not_found: a missing document and an out-of-scope one are indistinguishable (R2.6)', () => {
    const strip = (file: string) => {
      const { status, body } = clone(load(file).response)
      delete body.error.details.request_id
      delete body.error.details.generation
      return { status, body }
    }
    expect(strip('read.not-found.json')).toEqual(strip('read.out-of-scope.json'))
  })

  it('ready: infrastructure not ready makes every KB not_ready/dependency, empty KBs included (r6)', () => {
    const res = clone(load('ready.dependency.json').response.body)
    res.knowledge_bases[1].search = { state: 'ready' }
    ok('ready.response', res, false)
    res.knowledge_bases[1].search = { state: 'not_ready', reason: 'index_building' }
    ok('ready.response', res, false)
    ok('ready.response', { ...res, knowledge_bases: [] }, false)
  })

  it('ready: KB-level reasons are the closed R8.4 set; operation_disabled exactly when the operation is disabled (§12 #19)', () => {
    const res = clone(load('ready.scoped.json').response.body)
    const withKb = (kb: any) => ({ ...res, knowledge_bases: [{ ...res.knowledge_bases[0], ...kb }] })
    ok('ready.response', withKb({ search: { state: 'banana' } }), false)
    ok('ready.response', withKb({ search: { state: 'not_ready', reason: 'whatever' } }), false)
    ok('ready.response', withKb({ search: { state: 'not_ready' } }), false)
    ok('ready.response', withKb({ search: { state: 'ready', reason: 'dependency' } }), false)
    ok('ready.response', withKb({ search: { state: 'not_ready', reason: 'operation_disabled' } }), false)
    // R8.4 (r8): dependency only when the infrastructure is not_ready.
    ok('ready.response', withKb({ search: { state: 'not_ready', reason: 'dependency' } }), false)
    const off = clone(load('ready.operation-disabled.json').response.body)
    for (const search of [{ state: 'ready' }, { state: 'not_ready' }, { state: 'not_ready', reason: 'index_building' }]) {
      ok('ready.response', { ...off, knowledge_bases: [{ ...off.knowledge_bases[0], search }] }, false)
    }
    ok('ready.response', { ...off, knowledge_bases: [{ ...off.knowledge_bases[0], read: { state: 'not_ready', reason: 'operation_disabled' } }] }, false)
  })
})
