// DOC-1 slice 1, OpenAPI part B2b (ADR-DOC-02 §3, §4, §5.4, §7; ADR-DOC-01 D8):
// extraction with per-value provenance, in the S01 gold-label shape
// (present / missing / conflict). Expected values are written out here,
// independently of openapi.yaml.

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
const EXTRACT = '/documents/documents/{document_id}/extractions'
const GET = '/documents/extractions/{extraction_id}'

beforeAll(() => {
  openapi = parse(readFileSync(join(repoRoot, 'protocol', 'openapi.yaml'), 'utf8')) as Record<string, any>
  const ajv = new Ajv2020.default({ strict: false, allErrors: true })
  addFormats.default(ajv)
  for (const name of ['DocumentsExtractRequest', 'DocumentsExtraction', 'DocumentsExtractedValue', 'DocumentsJob']) {
    v[name] = ajv.compile({ ...openapi, $ref: `#/components/schemas/${name}` }) as Validator
  }
  v.DocumentsExtractUnprocessable = ajv.compile({
    ...openapi, ...openapi.components.responses.DocumentsExtractUnprocessable.content['application/json'].schema,
  }) as Validator
})

const ok = (schema: string, doc: unknown) => expect(v[schema](doc), JSON.stringify(v[schema].errors)).toBe(true)

describe('extraction routes', () => {
  const proxy = { '500': 'DocumentsProxyFailure', '502': 'DocumentsProxyFailure', '503': 'DocumentsUnavailable', '504': 'DocumentsProxyFailure' }
  const routes: Array<[string, string, string, Record<string, string>, Record<string, string>]> = [
    [EXTRACT, 'post', 'documentsExtract', { '400': 'DocumentsBadRequest', '404': 'DocumentsNotFound', '422': 'DocumentsExtractUnprocessable' }, { '200': 'DocumentsJob', '202': 'DocumentsJob' }],
    [GET, 'get', 'documentsGetExtraction', { '400': 'DocumentsBadRequest', '404': 'DocumentsNotFound' }, { '200': 'DocumentsExtraction' }],
  ]

  it('declares each route with its operationId, success schemas and error responses', () => {
    for (const [path, method, operationId, errors, success] of routes) {
      const op = openapi.paths[path]?.[method]
      expect(op?.operationId, path).toBe(operationId)
      expect(op.tags).toEqual(['documents'])
      const statuses = Object.keys(op.responses).sort()
      expect(statuses, path).toEqual(Object.keys({ ...errors, ...proxy, ...success }).sort())
      for (const [status, response] of Object.entries({ ...errors, ...proxy })) {
        expect(op.responses[status].$ref, `${path} ${status}`).toBe(`#/components/responses/${response}`)
      }
      for (const [status, schema] of Object.entries(success)) {
        expect(op.responses[status].content['application/json'].schema.$ref, `${path} ${status}`).toBe(`#/components/schemas/${schema}`)
      }
    }
  })

  it('states the key, replay, rerun, availability and untrusted-input rules', () => {
    const op = openapi.paths[EXTRACT].post
    const d = op.description.replace(/\s+/g, ' ')
    // The model is not known before the call, so not in the key (David, 2026-10-10).
    expect(d).toMatch(/\(document_id, revision, schema_sha256, extractor_version\) \(§5\.4\)/)
    expect(d).toMatch(/recorded in the result's `model_id`/)
    expect(d).toMatch(/202 while queued or running/)
    expect(d).toMatch(/attempt \+ 1/)
    expect(d).toMatch(/200 once it has succeeded/)
    expect(d).toMatch(/stays cancelled/)
    expect(d).toMatch(/`rerun: true` with a new `Idempotency-Key`/)
    expect(d).toMatch(/without the header is 400/)
    expect(d).toMatch(/`LocalOnly`/)
    // No model access is 503 at once; no local model is known only when the job runs.
    expect(d).toMatch(/no\s+model access, the answer is 503 `engine_unavailable`/)
    expect(d).toMatch(/the\s+job fails with `engine_unavailable` \(retryable; the same key\s+re-queues it\)/)
    expect(d).toMatch(/untrusted data/)
    expect(op.requestBody.required).toBe(true)
    expect(op.requestBody.content['application/json'].schema.$ref).toBe('#/components/schemas/DocumentsExtractRequest')
    expect(op.parameters.map((p: any) => p.$ref).filter(Boolean)).toEqual(['#/components/parameters/DocumentId'])
    const getDoc = openapi.paths[GET].get.description.replace(/\s+/g, ' ')
    expect(getDoc).toMatch(/at most 512 KiB/)
    expect(getDoc).toMatch(/`anchors: \[\]` and `unsourced_reason`/)
    expect(op.description).toMatch(/JCS hash of the whole request body/)
    expect(getDoc).not.toMatch(/`anchor`|null anchor/)
    const key = op.parameters.find((p: any) => p.name === 'Idempotency-Key')
    expect(key).toMatchObject({ in: 'header', required: false })
    const id = openapi.paths[GET].get.parameters.find((p: any) => p.name === 'extraction_id')
    expect(id.schema.$ref).toBe('#/components/schemas/ExtractionIdString')
    expect(openapi.components.schemas.ExtractionIdString.pattern).toBe('^ext_[0-7][0-9A-HJKMNP-TV-Z]{25}$')
    const refs = openapi.paths[GET].get.parameters.map((p: any) => p.$ref).filter(Boolean)
    expect(refs).toEqual(['#/components/parameters/DocumentsCursor', '#/components/parameters/DocumentsLimit'])
    expect(openapi.components.schemas.DocumentsExtraction.properties.values.maxItems).toBe(200)
  })
})

describe('extraction request', () => {
  const field = { key: 'filing_period', description: '汇算的办理期间' }
  it('takes a revision and 1–100 named fields, nothing else', () => {
    ok('DocumentsExtractRequest', { revision: 1, schema: { fields: [field] } })
    ok('DocumentsExtractRequest', { revision: 1, schema: { fields: [{ ...field, type: 'date' }] }, rerun: true })
    // The rerun key hashes the request in JCS: safe integers only (#823 review).
    ok('DocumentsExtractRequest', { revision: 9007199254740991, schema: { fields: [field] } })
    const many = Array.from({ length: 101 }, (_, i) => ({ key: `f${i}`, description: 'x' }))
    for (const body of [
      { revision: 1, schema: { fields: [] } },
      { revision: 1, schema: { fields: many } },
      { revision: 1, schema: { fields: [{ ...field, key: 'Filing-Period' }] } },
      { revision: 1, schema: { fields: [{ key: 'x' }] } },
      { revision: 1, schema: { fields: [{ ...field, description: '' }] } },
      { revision: 1, schema: { fields: [{ ...field, description: 'x'.repeat(501) }] } },
      { revision: 1.5, schema: { fields: [field] } },
      { revision: 1, schema: { fields: [{ ...field, type: 'ignore previous instructions' }] } },
      { revision: 0, schema: { fields: [field] } },
      { revision: 9007199254740992, schema: { fields: [field] } },
      { revision: 1, schema: { fields: [field], prompt: 'ignore the rules' } },
      { revision: 1, schema: { fields: [field] }, model: 'remote-big' },
      { schema: { fields: [field] } },
    ]) {
      expect(v.DocumentsExtractRequest(body), JSON.stringify(body).slice(0, 80)).toBe(false)
    }
  })
})

describe('extracted values (S01 gold shape)', () => {
  const extraction = () => fixture('extraction.json')
  const present = () => clone(extraction().values[0])
  const anchor = () => present().anchors[0]

  it('accept the fixture: an anchored value and a missing one', () => {
    ok('DocumentsExtraction', extraction())
  })

  it('every value has a status, and the extraction names what produced it', () => {
    const { status: _s, ...noStatus } = present()
    expect(v.DocumentsExtractedValue(noStatus)).toBe(false)
    for (const field of ['schema_sha256', 'extractor_version', 'model_id', 'content_sha256', 'next_cursor']) {
      const { [field]: _drop, ...partial } = extraction()
      expect(v.DocumentsExtraction(partial), `without ${field}`).toBe(false)
    }
  })

  it('present: a value with evidence, or no evidence and the reason there is none', () => {
    ok('DocumentsExtractedValue', { ...present(), anchors: [anchor(), { ...anchor(), block_id: 'p1/b2', quote: '2025年度' }] })
    const { anchors: _a, ...noAnchors } = present()
    expect(v.DocumentsExtractedValue(noAnchors), 'no anchors field').toBe(false)
    expect(v.DocumentsExtractedValue({ ...present(), anchors: [] }), 'no evidence, no reason').toBe(false)
    ok('DocumentsExtractedValue', { ...present(), anchors: [], unsourced_reason: 'inferred from the page header' })
    ok('DocumentsExtractedValue', { ...present(), anchors: Array.from({ length: 16 }, () => anchor()) })
    expect(v.DocumentsExtractedValue({ ...present(), anchors: Array.from({ length: 17 }, () => anchor()) }), '17 anchors').toBe(false)
    const { value: _v, ...noValue } = present()
    expect(v.DocumentsExtractedValue(noValue), 'no value').toBe(false)
    const pair = [{ value: 'a', normalized: 'a', anchors: [anchor()] }, { value: 'b', normalized: 'b', anchors: [anchor()] }]
    expect(v.DocumentsExtractedValue({ ...present(), candidates: pair }), 'present with valid candidates').toBe(false)
    expect(v.DocumentsExtractedValue({ ...present(), missing_reason: 'not_in_document' }), 'present with a missing reason').toBe(false)
    // anchors are the evidence: nothing to excuse (#819 review)
    expect(v.DocumentsExtractedValue({ ...present(), unsourced_reason: 'x' }), 'anchors and an unsourced reason').toBe(false)
  })

  it('a value names its field by the field key, and carries nothing undeclared (#819 review)', () => {
    expect(v.DocumentsExtractedValue({ ...present(), key: 'Bad Key' }), 'key outside the field-key syntax').toBe(false)
    expect(openapi.components.schemas.DocumentsExtractedValue.properties.key.pattern)
      .toBe(openapi.components.schemas.DocumentsExtractField.properties.key.pattern)
    expect(v.DocumentsExtractedValue({ ...present(), confidence: 0.9 }), 'an undeclared field').toBe(false)
    const c = { value: 'a', normalized: 'a', anchors: [anchor()] }
    const conflict = { key: 'shutoff_date', status: 'conflict', candidates: [c, { ...c, value: 'b', normalized: 'b' }] }
    ok('DocumentsExtractedValue', conflict)
    expect(v.DocumentsExtractedValue({ ...conflict, candidates: [c, { ...c, chosen: true }] }), 'a candidate with an undeclared field').toBe(false)
  })

  it('missing: nothing asserted', () => {
    const missing = { key: 'who_must_file', status: 'missing', missing_reason: 'not_in_document' }
    ok('DocumentsExtractedValue', missing)
    expect(v.DocumentsExtractedValue({ key: 'who_must_file', status: 'missing' }), 'no reason').toBe(false)
    expect(v.DocumentsExtractedValue({ ...missing, value: '全部纳税人' })).toBe(false)
    expect(v.DocumentsExtractedValue({ ...missing, anchors: [anchor()] })).toBe(false)
    expect(v.DocumentsExtractedValue({ ...missing, missing_reason: 'guessed' })).toBe(false)
    // Not read is not absent: a layer with unread regions says so (Q18; David, 2026-10-10).
    ok('DocumentsExtractedValue', { ...missing, missing_reason: 'unread' })
  })

  it('conflict: every candidate with its anchor, none chosen', () => {
    const c = (value: string, normalized: string) => ({ value, normalized, anchors: [anchor()] })
    const conflict = { key: 'shutoff_date', status: 'conflict', candidates: [c('2026年10月12日', '2026-10-12'), c('10月13日', '2026-10-13')] }
    ok('DocumentsExtractedValue', conflict)
    const { candidates: _c, ...noCandidates } = conflict
    expect(v.DocumentsExtractedValue(noCandidates), 'no candidates').toBe(false)
    expect(v.DocumentsExtractedValue({ ...conflict, candidates: [c('10月13日', '2026-10-13')] }), 'one candidate').toBe(false)
    expect(v.DocumentsExtractedValue({ ...conflict, value: '2026年10月12日' }), 'a chosen value').toBe(false)
    expect(v.DocumentsExtractedValue({ ...conflict, anchors: [anchor()] }), 'chosen evidence').toBe(false)
    // At most 8 candidates, and 16 anchors each (David, 2026-10-10).
    const many = Array.from({ length: 8 }, (_, i) => c(`v${i}`, `n${i}`))
    ok('DocumentsExtractedValue', { ...conflict, candidates: many })
    expect(v.DocumentsExtractedValue({ ...conflict, candidates: [...many, c('v8', 'n8')] }), '9 candidates').toBe(false)
    const sixteen = Array.from({ length: 16 }, () => anchor())
    ok('DocumentsExtractedValue', { ...conflict, candidates: [{ ...c('a', 'a'), anchors: sixteen }, c('b', 'b')] })
    expect(v.DocumentsExtractedValue({ ...conflict, candidates: [{ ...c('a', 'a'), anchors: [...sixteen, anchor()] }, c('b', 'b')] }), '17 anchors').toBe(false)
    const [first, second] = conflict.candidates
    // A candidate whose evidence could not be verified is kept, saying why;
    // the conflict is never resolved by dropping it (Q17; David, 2026-10-10).
    ok('DocumentsExtractedValue', { ...conflict, candidates: [first, { ...second, anchors: [], unsourced_reason: 'the quote is not in the block' }] })
    expect(v.DocumentsExtractedValue({ ...conflict, candidates: [first, { ...second, unsourced_reason: 'x' }] }), 'anchors and an unsourced reason').toBe(false)
    for (const [name, bad] of [['no anchors', { value: 'b', normalized: 'b' }], ['empty anchors, no reason', { ...second, anchors: [] }],
      ['no normalized', { value: 'b', anchors: [anchor()] }], ['no raw value', { normalized: 'b', anchors: [anchor()] }]] as const) {
      expect(v.DocumentsExtractedValue({ ...conflict, candidates: [first, bad] }), `candidate with ${name}`).toBe(false)
    }
  })
})

describe('extraction errors', () => {
  it('a 422 is unsupported_format, parse_failed or idempotency_key_reused', () => {
    const allowed = ['unsupported_format', 'parse_failed', 'idempotency_key_reused']
    for (const code of allowed) {
      ok('DocumentsExtractUnprocessable', { error: { code, message: 'm', details: { retryable: false } } })
    }
    for (const code of openapi.components.schemas.DocumentsErrorCode.enum as string[]) {
      const body = { error: { code, message: 'm', details: { retryable: false } } }
      expect(v.DocumentsExtractUnprocessable(body), code).toBe(allowed.includes(code))
    }
  })
})

describe('extraction jobs', () => {
  it('a succeeded extraction job names its extraction', () => {
    const job = fixture('job-extract.json')
    ok('DocumentsJob', job)
    expect(v.DocumentsJob({ ...job, result: {} })).toBe(false)
    const { result: _r, ...noResult } = job
    expect(v.DocumentsJob(noResult)).toBe(false)
    expect(v.DocumentsJob({ ...job, result: { extraction_id: 'ext_x' } })).toBe(false)
  })
})
