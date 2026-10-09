// DOC-1 slice 1, OpenAPI part B2a-1 (ADR-DOC-02 §4, §6): page render. Expected
// values are written out here, independently of openapi.yaml.

import { beforeAll, describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import Ajv2020 from 'ajv/dist/2020.js'
import addFormats from 'ajv-formats'
import { parse } from 'yaml'

type Validator = ((doc: unknown) => boolean) & { errors?: unknown }
const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')
let openapi: Record<string, any>
const v: Record<string, Validator> = {}
const PAGE = '/documents/documents/{document_id}/revisions/{revision}/pages/{page}'

beforeAll(() => {
  openapi = parse(readFileSync(join(repoRoot, 'protocol', 'openapi.yaml'), 'utf8')) as Record<string, any>
  const ajv = new Ajv2020.default({ strict: false, allErrors: true })
  addFormats.default(ajv)
  for (const name of ['DocumentsTooLarge', 'DocumentsRenderUnprocessable']) {
    v[name] = ajv.compile({ ...openapi, ...openapi.components.responses[name].content['application/json'].schema }) as Validator
  }
})

const err = (code: string) => ({ error: { code, message: 'm', details: { retryable: false } } })

describe('page render', () => {
  it('declares the route with its operationId, tag and every error response', () => {
    const op = openapi.paths[PAGE]?.get
    expect(op?.operationId).toBe('documentsRenderPage')
    expect(op.tags).toEqual(['documents'])
    const errors: Record<string, string> = {
      '400': 'DocumentsBadRequest', '404': 'DocumentsNotFound', '413': 'DocumentsTooLarge', '422': 'DocumentsRenderUnprocessable',
      '500': 'DocumentsProxyFailure', '502': 'DocumentsProxyFailure', '503': 'DocumentsUnavailable', '504': 'DocumentsProxyFailure',
    }
    expect(Object.keys(op.responses).filter((k) => !k.startsWith('2')).sort()).toEqual(Object.keys(errors).sort())
    for (const [status, response] of Object.entries(errors)) {
      expect(op.responses[status].$ref, status).toBe(`#/components/responses/${response}`)
    }
  })

  it('states what is not a page: past the last one, or a flowing format (#815 review)', () => {
    const d: string = openapi.paths[PAGE].get.description
    expect(d).toMatch(/page past\s+the revision's last page is 404 `not_found`/)
    expect(d).toMatch(/flowing format \(DOCX\)\s+has no pages and is 422 `unsupported_format`/)
    // ADR §4 agrees: the OS does not tile on its own; the client asks for regions.
    const adr = readFileSync(join(repoRoot, 'docs', 'documenting', 'adr', 'ADR-DOC-02-operation-contract.md'), 'utf8')
    expect(adr).toMatch(/降到 0\.25 仍超出则 413，由客户端用 `region` 分块请求/)
    expect(adr).not.toMatch(/仍超出则按瓦片分块/)
  })

  it('renders PNG and reports the scale it used', () => {
    const ok200 = openapi.paths[PAGE].get.responses['200']
    expect(Object.keys(ok200.content)).toEqual(['image/png'])
    expect(ok200.headers['Documents-Render-Scale']).toMatchObject({ required: true, schema: { type: 'number', minimum: 0.25, maximum: 4 } })
  })

  it('takes a document, a revision, a 1-based page, a bounded scale and an optional region', () => {
    const params = openapi.paths[PAGE].get.parameters
    const refs = params.map((x: any) => x.$ref).filter(Boolean)
    expect(refs).toEqual(['#/components/parameters/DocumentId', '#/components/parameters/RevisionNumber'])
    expect(openapi.components.parameters.RevisionNumber.schema).toEqual({ type: 'integer', minimum: 1 })
    const p = Object.fromEntries(params.filter((x: any) => x.name).map((x: any) => [x.name, x]))
    expect(p.page.schema).toEqual({ type: 'integer', minimum: 1 })
    expect(p.scale.schema).toEqual({ type: 'number', minimum: 0.25, maximum: 4, default: 1 })
    const region = new RegExp(p.region.schema.pattern)
    for (const good of ['0,0,612,792', '10.5,20,100.25,50']) expect(region.test(good), good).toBe(true)
    for (const bad of ['1,2,3', '-1,0,1,1', 'a,b,c,d', '1,2,3,4,5']) expect(region.test(bad), bad).toBe(false)
  })

  it('413 is payload_too_large only; a render 422 is unsupported_format or parse_failed', () => {
    const all = openapi.components.schemas.DocumentsErrorCode.enum as string[]
    const expected: Record<string, string[]> = {
      DocumentsTooLarge: ['payload_too_large'],
      DocumentsRenderUnprocessable: ['unsupported_format', 'parse_failed'],
    }
    for (const [response, codes] of Object.entries(expected)) {
      for (const code of all) expect(v[response](err(code)), `${response} ${code}`).toBe(codes.includes(code))
    }
  })
})
