// DOC-1 slice 1 (ADR-DOC-02 §3, §4, §6, §8): representative Documenting OS
// responses must match the public OpenAPI schemas, and the schemas must
// reject the shapes the ADR forbids. Expected values are written out here,
// independently of openapi.yaml, so an edit to either side shows up.

import { beforeAll, describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import Ajv2020 from 'ajv/dist/2020.js'
import addFormats from 'ajv-formats'
import { parse } from 'yaml'

type Validator = ((doc: unknown) => boolean) & { errors?: unknown }
const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')
const fixture = (name: string): any =>
  JSON.parse(readFileSync(join(repoRoot, 'protocol', 'fixtures', 'documents', name), 'utf8'))
const clone = <T>(x: T): T => JSON.parse(JSON.stringify(x))
let openapi: Record<string, any>
const v: Record<string, Validator> = {}

// ADR-DOC-02 §6, in table order.
const ADR_CODES = [
  'invalid_request', 'not_found', 'payload_too_large', 'unsupported_format', 'parse_failed', 'partial_parse',
  'idempotency_key_reused', 'change_set_incomplete', 'upload_checksum_mismatch', 'knowledge_disabled',
  'revision_conflict', 'change_set_stale', 'change_set_committed', 'upload_offset_mismatch', 'stale_index',
  'permission_denied', 'knowledge_unavailable', 'engine_unavailable', 'storage_unavailable',
]

beforeAll(() => {
  openapi = parse(readFileSync(join(repoRoot, 'protocol', 'openapi.yaml'), 'utf8')) as Record<string, any>
  const ajv = new Ajv2020.default({ strict: false, allErrors: true })
  addFormats.default(ajv)
  const schemas = ['DocumentsCapabilities', 'DocumentList', 'DocumentDetail', 'DocumentsError', 'ModuleProxyError']
  for (const name of schemas) v[name] = ajv.compile({ ...openapi, $ref: `#/components/schemas/${name}` }) as Validator
  const response = (name: string) => ({
    ...openapi,
    ...openapi.components.responses[name].content['application/json'].schema,
  })
  v.Unavailable503 = ajv.compile(response('DocumentsUnavailable')) as Validator
  v.ProxyFailure = ajv.compile(response('DocumentsProxyFailure')) as Validator
})

const ok = (schema: string, doc: unknown) => expect(v[schema](doc), JSON.stringify(v[schema].errors)).toBe(true)
const err = (code: string, details: Record<string, unknown>) => ({ error: { code, message: 'm', details } })

describe('Documenting OS OpenAPI: representative responses', () => {
  it('accepts the fixtures', () => {
    ok('DocumentsCapabilities', fixture('capabilities.json'))
    ok('DocumentList', fixture('list.json'))
    ok('DocumentDetail', fixture('detail.json'))
    ok('DocumentsError', fixture('error.json'))
  })

  it('accepts a populated engine list and an available operation', () => {
    const caps = fixture('capabilities.json')
    caps.storage.state = 'ready'
    caps.engines = [{ id: 'apple-pdfkit', kind: 'parse', formats: ['pdf'], state: 'ready', version: '26.6' }]
    caps.operations[0] = { op: 'upload', available: true, reason: null }
    ok('DocumentsCapabilities', caps)
    caps.engines[0].kind = 'magic'
    expect(v.DocumentsCapabilities(caps)).toBe(false)
  })
})

describe('Documenting OS OpenAPI: ADR rules', () => {
  it('lists exactly the ADR-DOC-02 §6 error codes', () => {
    expect(openapi.components.schemas.DocumentsErrorCode.enum).toEqual(ADR_CODES)
  })

  it('pages with limit 1..200, default 50 (§4)', () => {
    expect(openapi.components.parameters.DocumentsLimit.schema).toEqual({ type: 'integer', minimum: 1, maximum: 200, default: 50 })
  })

  it('declares the three slice-1 routes with their response schemas and proxy failures', () => {
    const expected: Record<string, [string, string]> = {
      '/documents/capabilities': ['documentsGetCapabilities', 'DocumentsCapabilities'],
      '/documents/documents': ['documentsListDocuments', 'DocumentList'],
      '/documents/documents/{document_id}': ['documentsGetDocument', 'DocumentDetail'],
    }
    for (const [path, [operationId, schema]] of Object.entries(expected)) {
      const op = openapi.paths[path]?.get
      expect(op?.operationId, path).toBe(operationId)
      expect(op.tags).toEqual(['documents'])
      expect(op.responses['200'].content['application/json'].schema.$ref).toBe(`#/components/schemas/${schema}`)
      for (const status of ['500', '502', '504']) {
        expect(op.responses[status]?.$ref, `${path} ${status}`).toBe('#/components/responses/DocumentsProxyFailure')
      }
      expect(op.responses['503'].$ref).toBe('#/components/responses/DocumentsUnavailable')
    }
  })

  it('requires a typed reason from the closed set when an operation is unavailable', () => {
    const caps = fixture('capabilities.json')
    for (const reason of [undefined, null, 'not ready yet']) {
      const bad = clone(caps)
      if (reason === undefined) delete bad.operations[0].reason
      else bad.operations[0].reason = reason
      expect(v.DocumentsCapabilities(bad), String(reason)).toBe(false)
    }
  })

  it('requires details.retryable and a code from the closed set', () => {
    const noRetryable = clone(fixture('error.json'))
    delete noRetryable.error.details.retryable
    expect(v.DocumentsError(noRetryable)).toBe(false)
    expect(v.DocumentsError(err('oops', { retryable: false }))).toBe(false)
  })

  it('enforces the code-specific details of §6', () => {
    const cases: Array<[string, Record<string, unknown>, boolean]> = [
      ['engine_unavailable', { retryable: true, engine: 'apple-vision' }, true],
      ['engine_unavailable', { retryable: true }, false],
      ['engine_unavailable', { retryable: false, engine: 'apple-vision' }, false],
      ['knowledge_unavailable', { retryable: true }, true],
      ['knowledge_unavailable', { retryable: false }, false],
      ['revision_conflict', { retryable: false, current_revision: 3 }, true],
      ['revision_conflict', { retryable: false }, false],
      ['change_set_committed', { retryable: false, revision: 2 }, true],
      ['change_set_committed', { retryable: false }, false],
      ['upload_offset_mismatch', { retryable: false, received_offset: 786432 }, true],
      ['upload_offset_mismatch', { retryable: false }, false],
    ]
    for (const [code, details, valid] of cases) {
      expect(v.DocumentsError(err(code, details)), `${code} ${JSON.stringify(details)}`).toBe(valid)
    }
  })

  it('keeps OS 503s and proxy 503s apart', () => {
    ok('Unavailable503', err('storage_unavailable', { retryable: true }))
    ok('Unavailable503', { error: { code: 'module_not_ready', message: 'm' } })
    // An OS availability code must carry retryable; the generic branch cannot absorb it.
    expect(v.Unavailable503({ error: { code: 'storage_unavailable', message: 'm' } })).toBe(false)
    // Other OS codes are not 503s.
    expect(v.Unavailable503(err('parse_failed', { retryable: false }))).toBe(false)
  })

  it('accepts proxy failures in the generic envelope but not OS-only codes', () => {
    ok('ProxyFailure', { error: { code: 'upstream_timeout', message: 'm' } })
    ok('ProxyFailure', { error: { code: 'request_abandoned', message: 'm' } })
    const shared = ['invalid_request', 'not_found', 'payload_too_large']
    for (const code of ADR_CODES) {
      expect(v.ModuleProxyError({ error: { code, message: 'm' } }), code).toBe(shared.includes(code))
    }
  })

  it('accepts only well-formed ULID document ids and sha256 addresses', () => {
    const detail = fixture('detail.json')
    ok('DocumentDetail', { ...clone(detail), document_id: 'doc_7ZZZZZZZZZZZZZZZZZZZZZZZZZ' })
    for (const id of ['01K74Z3QJ8V5N2W9RTX6YB4MCD', 'doc_8ZZZZZZZZZZZZZZZZZZZZZZZZZ', 'doc_01K74Z3QJ8V5N2W9RTX6YB4MCI', 'doc_01k74z3qj8v5n2w9rtx6yb4mcd']) {
      expect(v.DocumentDetail({ ...clone(detail), document_id: id }), id).toBe(false)
    }
    const badSha = clone(detail)
    badSha.head.content_sha256 = 'sha256:ABC'
    expect(v.DocumentDetail(badSha)).toBe(false)
  })

  it('requires the head revision and real timestamps', () => {
    const noHead = clone(fixture('detail.json'))
    delete noHead.head
    expect(v.DocumentDetail(noHead)).toBe(false)
    const list = clone(fixture('list.json'))
    list.documents[0].created_at = 'not-a-date'
    expect(v.DocumentList(list)).toBe(false)
  })
})
