// DOC-1 slice 1, OpenAPI part B1a (ADR-DOC-02 §5.4, §5.6, §6): uploads, the
// narrowed 400/404/409/422 envelopes (#803 review) and the storage_unavailable
// cause rule (#806). Expected values are written out here,
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
  for (const name of ['DocumentsUpload', 'DocumentsUploadRequest', 'DocumentsError']) {
    v[name] = ajv.compile({ ...openapi, $ref: `#/components/schemas/${name}` }) as Validator
  }
  for (const name of ['DocumentsBadRequest', 'DocumentsNotFound', 'DocumentsConflict', 'DocumentsUploadUnprocessable', 'DocumentsUnavailable']) {
    v[name] = ajv.compile({ ...openapi, ...openapi.components.responses[name].content['application/json'].schema }) as Validator
  }
})

const ok = (schema: string, doc: unknown) => expect(v[schema](doc), JSON.stringify(v[schema].errors)).toBe(true)
const err = (code: string, details: Record<string, unknown> = { retryable: false }) => ({ error: { code, message: 'm', details } })
const kernelErr = (code: string) => ({ error: { code, message: 'm' } })

describe('upload routes', () => {
  const proxy = { '500': 'DocumentsProxyFailure', '502': 'DocumentsProxyFailure', '503': 'DocumentsUnavailable', '504': 'DocumentsProxyFailure' }
  const routes: Array<[string, string, string, Record<string, string>]> = [
    ['/documents/uploads', 'post', 'documentsCreateUpload', { '400': 'DocumentsBadRequest', '422': 'DocumentsUploadUnprocessable' }],
    ['/documents/uploads/{upload_id}/chunks', 'post', 'documentsAppendUploadChunk',
      { '400': 'DocumentsBadRequest', '404': 'DocumentsNotFound', '409': 'DocumentsConflict', '413': 'PayloadTooLarge' }],
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

  it('requires an Idempotency-Key to create an upload', () => {
    const params = openapi.paths['/documents/uploads'].post.parameters.map((p: any) => p.$ref)
    expect(params).toContain('#/components/parameters/DocumentsIdempotencyKey')
    expect(openapi.components.parameters.DocumentsIdempotencyKey).toMatchObject({ in: 'header', required: true })
  })

  it('takes chunks as raw bytes of at most 768 KiB with offset and hash headers', () => {
    const op = openapi.paths['/documents/uploads/{upload_id}/chunks'].post
    expect(op.requestBody.content['application/octet-stream'].schema.maxLength).toBe(768 * 1024)
    const headers = op.parameters.filter((p: any) => p.in === 'header')
    expect(headers.map((p: any) => [p.name, p.required])).toEqual([['Upload-Offset', true], ['Chunk-Sha256', true]])
  })

  it('declares each success status with its schema (§5.4, §7 replay)', () => {
    const success: Array<[string, string, Record<string, string>]> = [
      ['/documents/uploads', 'post', { '200': 'DocumentsUpload', '201': 'DocumentsUpload' }],
      ['/documents/uploads/{upload_id}/chunks', 'post', { '200': 'DocumentsUpload' }],
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

  it('states the chunk check order: 400 for a new chunk, 409 for a replayed range (§5.6)', () => {
    const d: string = openapi.paths['/documents/uploads/{upload_id}/chunks'].post.description
    expect(d).toMatch(/new chunk[^.]*400 `invalid_request`/)
    expect(d).toMatch(/already received[^.]*409/)
  })

  it('requires the request bodies and constrains ids, headers and the chunk size', () => {
    for (const path of ['/documents/uploads', '/documents/uploads/{upload_id}/chunks']) {
      expect(openapi.paths[path].post.requestBody.required, path).toBe(true)
    }
    const p = openapi.components.parameters
    expect(p.UploadId.schema.pattern).toBe('^upl_[0-7][0-9A-HJKMNP-TV-Z]{25}$')
    expect(p.DocumentsIdempotencyKey.schema).toEqual({ type: 'string', minLength: 1, maxLength: 200 })
    const headers = openapi.paths['/documents/uploads/{upload_id}/chunks'].post.parameters.filter((x: any) => x.in === 'header')
    expect(headers[0].schema).toEqual({ type: 'integer', minimum: 0 })
    expect(headers[1].schema.$ref).toBe('#/components/schemas/Sha256Address')
  })
})

describe('upload schemas', () => {
  it('accept the fixtures', () => {
    ok('DocumentsUpload', fixture('upload.json'))
  })

  it('never take a path as the upload filename, nor unknown fields', () => {
    const base = { total_size: 10, sha256: fixture('upload.json').sha256 }
    ok('DocumentsUploadRequest', { ...base, filename: '通告.pdf' })
    for (const filename of ['/Users/x/a.pdf', 'dir/a.pdf', 'C:\\a.pdf', '']) {
      expect(v.DocumentsUploadRequest({ ...base, filename }), filename).toBe(false)
    }
    expect(v.DocumentsUploadRequest({ ...base, path: '/tmp/a.pdf' })).toBe(false)
  })

  it('reject bad ids and sizes', () => {
    const up = fixture('upload.json')
    expect(v.DocumentsUpload({ ...up, upload_id: 'upl_8ZZZZZZZZZZZZZZZZZZZZZZZZZ' })).toBe(false)
    expect(v.DocumentsUploadRequest({ total_size: 0, sha256: up.sha256 })).toBe(false)
    expect(v.DocumentsUploadRequest({ total_size: 10 })).toBe(false)
  })
})

describe('narrowed error envelopes', () => {
  it('a 400 is invalid_request, from the OS or the kernel, and nothing else', () => {
    ok('DocumentsBadRequest', err('invalid_request'))
    ok('DocumentsBadRequest', kernelErr('invalid_request'))
    ok('DocumentsBadRequest', kernelErr('invalid_request_path'))
    // An OS envelope with broken details cannot slip through the kernel branch.
    expect(v.DocumentsBadRequest(err('invalid_request', { retryable: 'no' }))).toBe(false)
    expect(v.DocumentsBadRequest({ error: { code: 'invalid_request', message: 'm', details: {} } })).toBe(false)
    expect(v.DocumentsBadRequest(err('storage_unavailable', { retryable: true, cause: 'locked' }))).toBe(false)
    expect(v.DocumentsBadRequest(kernelErr('not_found'))).toBe(false)
  })

  it('a 404 is not_found, from the OS or the kernel, and nothing else', () => {
    ok('DocumentsNotFound', err('not_found'))
    ok('DocumentsNotFound', kernelErr('not_found'))
    expect(v.DocumentsNotFound({ error: { code: 'not_found', message: 'm', details: {} } })).toBe(false)
    expect(v.DocumentsNotFound(err('invalid_request'))).toBe(false)
    expect(v.DocumentsNotFound(kernelErr('invalid_request'))).toBe(false)
  })

  it('a 409 carries received_offset and an upload 422 is idempotency_key_reused only', () => {
    ok('DocumentsConflict', err('upload_offset_mismatch', { retryable: false, received_offset: 786432 }))
    expect(v.DocumentsConflict(err('upload_offset_mismatch'))).toBe(false)
    ok('DocumentsUploadUnprocessable', err('idempotency_key_reused'))
    expect(v.DocumentsUploadUnprocessable(err('upload_checksum_mismatch')), 'checksum is checked at import, not upload').toBe(false)
  })
})

describe('storage_unavailable (ADR-DOC-02 §6, #806)', () => {
  it('needs a cause, and only lock contention is retryable', () => {
    const retryableBy: Record<string, boolean> = { locked: true, busy: true, corrupt: false, not_writable: false, disk_full: false }
    for (const [cause, retryable] of Object.entries(retryableBy)) {
      for (const flag of [true, false]) {
        const body = err('storage_unavailable', { retryable: flag, cause })
        expect(v.DocumentsError(body), `${cause} retryable=${flag}`).toBe(flag === retryable)
        expect(v.DocumentsUnavailable(body), `503 ${cause} retryable=${flag}`).toBe(flag === retryable)
      }
    }
    for (const details of [{ retryable: true }, { retryable: false, cause: 'cosmic_rays' }, { cause: 'locked' }]) {
      expect(v.DocumentsError(err('storage_unavailable', details as any)), JSON.stringify(details)).toBe(false)
    }
  })
})
