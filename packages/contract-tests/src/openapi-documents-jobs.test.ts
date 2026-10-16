// DOC-1 slice 1, OpenAPI part B1b (ADR-DOC-02 §4, §5.4, §7): imports and
// jobs. Uploads and the shared error envelopes are in
// openapi-documents-uploads.test.ts. Expected values are written out here,
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
let openapi: Record<string, any>
const v: Record<string, Validator> = {}

beforeAll(() => {
  openapi = parse(readFileSync(join(repoRoot, 'protocol', 'openapi.yaml'), 'utf8')) as Record<string, any>
  const ajv = new Ajv2020.default({ strict: false, allErrors: true })
  addFormats.default(ajv)
  for (const name of ['DocumentsImportRequest', 'DocumentsJob']) {
    v[name] = ajv.compile({ ...openapi, $ref: `#/components/schemas/${name}` }) as Validator
  }
  for (const name of ['DocumentsImportUnprocessable']) {
    v[name] = ajv.compile({ ...openapi, ...openapi.components.responses[name].content['application/json'].schema }) as Validator
  }
})

const ok = (schema: string, doc: unknown) => expect(v[schema](doc), JSON.stringify(v[schema].errors)).toBe(true)
const err = (code: string, details: Record<string, unknown> = { retryable: false }) => ({ error: { code, message: 'm', details } })

describe('import and job routes', () => {
  const proxy = { '500': 'DocumentsProxyFailure', '502': 'DocumentsProxyFailure', '503': 'DocumentsUnavailable', '504': 'DocumentsProxyFailure' }
  const routes: Array<[string, string, string, Record<string, string>]> = [
    ['/documents/imports', 'post', 'documentsImport', { '400': 'DocumentsBadRequest', '404': 'DocumentsNotFound', '422': 'DocumentsImportUnprocessable' }],
    ['/documents/jobs/{job_id}', 'get', 'documentsGetJob', { '400': 'DocumentsBadRequest', '404': 'DocumentsNotFound' }],
    ['/documents/jobs/{job_id}/cancel', 'post', 'documentsCancelJob', { '400': 'DocumentsBadRequest', '404': 'DocumentsNotFound' }],
    ['/documents/jobs/{job_id}/retry', 'post', 'documentsRetryJob', { '400': 'DocumentsBadRequest', '404': 'DocumentsNotFound' }],
  ]

  it('declares each route with its operationId, tag and error responses', () => {
    for (const [path, method, operationId, errors] of routes) {
      const op = openapi.paths[path]?.[method]
      expect(op?.operationId, `${method} ${path}`).toBe(operationId)
      expect(op.tags).toEqual(['documents'])
      for (const [status, response] of Object.entries({ ...errors, ...proxy })) {
        expect(op.responses[status]?.$ref, `${path} ${status}`).toBe(`#/components/responses/${response}`)
      }
    }
  })

  it('declares each success status with its schema (§5.4, §7 replay)', () => {
    const success: Array<[string, string, Record<string, string>]> = [
      ['/documents/imports', 'post', { '200': 'DocumentsJob', '202': 'DocumentsJob' }],
      ['/documents/jobs/{job_id}', 'get', { '200': 'DocumentsJob' }],
      ['/documents/jobs/{job_id}/cancel', 'post', { '200': 'DocumentsJob' }],
      ['/documents/jobs/{job_id}/retry', 'post', { '200': 'DocumentsJob' }],
    ]
    for (const [path, method, statuses] of success) {
      const responses = openapi.paths[path][method].responses
      const declared = Object.keys(responses).filter((k) => k.startsWith('2')).sort()
      expect(declared, path).toEqual(Object.keys(statuses).sort())
      for (const [status, schema] of Object.entries(statuses)) {
        expect(responses[status].content['application/json'].schema.$ref, `${path} ${status}`).toBe(`#/components/schemas/${schema}`)
      }
    }
  })

  it('states the import replay statuses (§7)', () => {
    const d: string = openapi.paths['/documents/imports'].post.description
    expect(d).toMatch(/202 while it is queued, running or\s+cancelling/)
    expect(d).toMatch(/attempt \+ 1/)
    expect(d).toMatch(/200 once it\s+has succeeded/)
    expect(d).toMatch(/stays cancelled/)
  })

  it('requires the import body and constrains job ids', () => {
    expect(openapi.paths['/documents/imports'].post.requestBody.required).toBe(true)
    expect(openapi.components.schemas.JobIdString.pattern).toBe('^job_[0-7][0-9A-HJKMNP-TV-Z]{25}$')
    expect(openapi.components.parameters.JobId.schema.$ref).toBe('#/components/schemas/JobIdString')
  })
})

describe('import and job schemas', () => {
  it('accept the fixtures', () => {
    ok('DocumentsJob', fixture('job-import.json'))
  })

  it('reject bad job ids, attempts and progress', () => {
    const job = fixture('job-import.json')
    expect(v.DocumentsJob({ ...job, attempt: 0 })).toBe(false)
    expect(v.DocumentsJob({ ...job, job_id: 'job_x' })).toBe(false)
    const progress = { stage: 'store', done: 1, total: 1, unit: 'file' }
    for (const missing of ['stage', 'done', 'total', 'unit']) {
      const { [missing]: _drop, ...partial } = progress as Record<string, unknown>
      expect(v.DocumentsJob({ ...job, progress: partial }), `progress without ${missing}`).toBe(false)
    }
    ok('DocumentsJob', { ...job, progress: { ...progress, total: null } })
  })

  it('require each terminal state to carry what it means (§7)', () => {
    const job = fixture('job-import.json')
    const failed = { ...job, status: 'failed', result: null }
    ok('DocumentsJob', { ...failed, error: { code: 'parse_failed', message: 'x' } })
    expect(v.DocumentsJob({ ...failed, error: null }), 'failed with a null error').toBe(false)
    const { error: _e, ...failedNoError } = failed
    expect(v.DocumentsJob(failedNoError), 'failed with no error field').toBe(false)
    expect(v.DocumentsJob({ ...failed, error: { code: 'cancelled', message: 'x' } }), 'failed as cancelled').toBe(false)
    const cancelled = { ...job, status: 'cancelled', result: null }
    ok('DocumentsJob', { ...cancelled, error: null })
    expect(v.DocumentsJob({ ...cancelled, error: { code: 'parse_failed', message: 'x' } })).toBe(false)
    for (const result of [null, {}, { document_id: job.result.document_id }, { ...job.result, revision: 2 }]) {
      expect(v.DocumentsJob({ ...job, result }), `succeeded import with ${JSON.stringify(result)}`).toBe(false)
    }
    const { result: _r, ...succeededNoResult } = job
    expect(v.DocumentsJob(succeededNoResult), 'succeeded import with no result field').toBe(false)
    expect(v.DocumentsJob({ ...job, status: 'running', error: { code: 'parse_failed', message: 'x' } })).toBe(false)
  })

  it('an interrupted job carries no error, and only a succeeded job has a result (#813 review)', () => {
    const job = fixture('job-import.json')
    const interrupted = { ...job, status: 'interrupted', result: null, error: null }
    ok('DocumentsJob', interrupted)
    for (const code of ['cancelled', 'parse_failed']) {
      expect(v.DocumentsJob({ ...interrupted, error: { code, message: 'x' } }), `interrupted with ${code}`).toBe(false)
    }
    for (const status of ['queued', 'running', 'cancelling', 'interrupted']) {
      expect(v.DocumentsJob({ ...job, status, error: null }), `${status} with a result`).toBe(false)
      ok('DocumentsJob', { ...job, status, error: null, result: null })
    }
    expect(v.DocumentsJob({ ...job, status: 'failed', error: { code: 'parse_failed', message: 'x' } }), 'failed with a result').toBe(false)
    expect(v.DocumentsJob({ ...job, status: 'cancelled', error: null }), 'cancelled with a result').toBe(false)
  })

  it('states the import, retry and chunk edge cases (#812, #813 reviews)', () => {
    const imp: string = openapi.paths['/documents/imports'].post.description
    expect(imp).toMatch(/queued, running or cancelling/)
    expect(imp).toMatch(/POST \/documents\/jobs\/\{job_id\}\/retry/)
    expect(imp).toMatch(/different `title` — including a `title`\s+added or left out — is 422 `idempotency_key_reused`/)
    expect(imp).toMatch(/24 h[\s\S]*404 `not_found`/)
    expect(imp).toMatch(/hash and\s+its format are checked before the job starts: 422\s+`upload_checksum_mismatch` or `unsupported_format`/)
    const retryOp = openapi.paths['/documents/jobs/{job_id}/retry'].post
    const retry: string = retryOp.description
    expect(retry).toMatch(/no idempotency key, so a retry whose outcome is unknown must not be\s+repeated blindly/)
    expect(retry).toMatch(/GET the job and compare\s+`attempt`/)
    expect(retry).not.toMatch(/the earlier\s+retry landed/)
    expect(retry).toMatch(/unchanged `attempt` does not prove the retry was lost/)
    expect(imp).toMatch(/an empty `title`\s+is 400, never the same as none/)
    expect(v.DocumentsImportRequest({ upload_id: 'upl_01K74Z3QJ8V5N2W9RTX6YB4MCD', title: '' }), 'empty title').toBe(false)
    expect((retryOp.parameters ?? []).map((p: any) => p.$ref)).not.toContain('#/components/parameters/DocumentsIdempotencyKey')
    const chunk: string = openapi.paths['/documents/uploads/{upload_id}/chunks'].post.description
    expect(chunk).toMatch(/complete or imported takes no new chunk: 409\s+`upload_offset_mismatch` with `details.received_offset`, decided\s+before the `total_size` and `Chunk-Sha256` checks/)
    expect(chunk).toMatch(/still receiving, a new chunk that would run past\s+`total_size` is 400/)
    expect(chunk).toMatch(/24 h after its last chunk, is 404 `not_found`/)
    expect(chunk).toMatch(/413 `payload_too_large`/)
  })

  it('keep job kind open for later slices but job status closed', () => {
    ok('DocumentsJob', { ...fixture('job-import.json'), kind: 'export' })
    expect(v.DocumentsJob({ ...fixture('job-import.json'), status: 'paused' })).toBe(false)
  })

  it('report cancellation only inside job.error, as code "cancelled"', () => {
    const job = { ...fixture('job-import.json'), status: 'cancelled', result: null, error: { code: 'cancelled', message: 'by user' } }
    ok('DocumentsJob', job)
    expect(v.DocumentsJob({ ...job, error: { code: 'oops', message: 'x' } })).toBe(false)
  })
})

describe('import errors', () => {
  it('an import 422 is one of its three codes', () => {
    for (const code of ['idempotency_key_reused', 'upload_checksum_mismatch', 'unsupported_format']) ok('DocumentsImportUnprocessable', err(code))
    expect(v.DocumentsImportUnprocessable(err('parse_failed'))).toBe(false)
  })
})
