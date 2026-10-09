// DOC-1 slice 1, OpenAPI part B2a-2 (ADR-DOC-02 §3, §4, §6): text and find,
// and the per-value provenance anchor. Page render is in
// openapi-documents-render.test.ts. Expected values are written out
// here, independently of openapi.yaml.

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

beforeAll(() => {
  openapi = parse(readFileSync(join(repoRoot, 'protocol', 'openapi.yaml'), 'utf8')) as Record<string, any>
  const ajv = new Ajv2020.default({ strict: false, allErrors: true })
  addFormats.default(ajv)
  for (const name of ['DocumentsAnchor', 'DocumentsTextPage', 'DocumentsFindRequest', 'DocumentsFindResult']) {
    v[name] = ajv.compile({ ...openapi, $ref: `#/components/schemas/${name}` }) as Validator
  }
  for (const name of ['DocumentsReadUnprocessable']) {
    v[name] = ajv.compile({ ...openapi, ...openapi.components.responses[name].content['application/json'].schema }) as Validator
  }
})

const ok = (schema: string, doc: unknown) => expect(v[schema](doc), JSON.stringify(v[schema].errors)).toBe(true)
const err = (code: string) => ({ error: { code, message: 'm', details: { retryable: false } } })
const TEXT = '/documents/documents/{document_id}/revisions/{revision}/text'
const FIND = '/documents/documents/{document_id}/find'
const DOCX = 'application/vnd.openxmlformats-officedocument.wordprocessingml.document'

describe('read routes', () => {
  const proxy = { '500': 'DocumentsProxyFailure', '502': 'DocumentsProxyFailure', '503': 'DocumentsUnavailable', '504': 'DocumentsProxyFailure' }
  const routes: Array<[string, string, string, Record<string, string>]> = [
    [TEXT, 'get', 'documentsReadText', { '400': 'DocumentsBadRequest', '404': 'DocumentsNotFound', '422': 'DocumentsReadUnprocessable' }],
    [FIND, 'post', 'documentsFind', { '400': 'DocumentsBadRequest', '404': 'DocumentsNotFound', '422': 'DocumentsReadUnprocessable' }],
  ]

  it('declares each route with its operationId, tag and error responses', () => {
    for (const [path, method, operationId, errors] of routes) {
      const op = openapi.paths[path]?.[method]
      expect(op?.operationId, path).toBe(operationId)
      expect(op.tags).toEqual(['documents'])
      const declared = Object.keys(op.responses).filter((k) => !k.startsWith('2')).sort()
      expect(declared, path).toEqual(Object.keys({ ...errors, ...proxy }).sort())
      for (const [status, response] of Object.entries({ ...errors, ...proxy })) {
        expect(op.responses[status].$ref, `${path} ${status}`).toBe(`#/components/responses/${response}`)
      }
    }
  })

  it('returns text pages and find results, with revision and paging parameters', () => {
    expect(openapi.paths[TEXT].get.responses['200'].content['application/json'].schema.$ref).toBe('#/components/schemas/DocumentsTextPage')
    expect(openapi.paths[FIND].post.responses['200'].content['application/json'].schema.$ref).toBe('#/components/schemas/DocumentsFindResult')
    expect(openapi.paths[FIND].post.requestBody.required).toBe(true)
    expect(openapi.paths[FIND].post.requestBody.content['application/json'].schema.$ref).toBe('#/components/schemas/DocumentsFindRequest')
    for (const path of [TEXT]) {
      const refs = openapi.paths[path].get.parameters.map((x: any) => x.$ref)
      expect(refs, path).toContain('#/components/parameters/DocumentId')
      expect(refs, path).toContain('#/components/parameters/RevisionNumber')
    }
    for (const schema of ['DocumentsTextPage', 'DocumentsFindResult']) {
      const list = schema === 'DocumentsTextPage' ? 'blocks' : 'matches'
      expect(openapi.components.schemas[schema].properties[list].maxItems, schema).toBe(200)
    }
    expect(openapi.components.parameters.RevisionNumber.schema).toEqual({ type: 'integer', minimum: 1 })
    expect(openapi.components.parameters.DocumentsRequireComplete.schema).toEqual({ type: 'boolean', default: false })
    const refs = openapi.paths[TEXT].get.parameters.map((x: any) => x.$ref).filter(Boolean)
    for (const ref of ['DocumentsCursor', 'DocumentsLimit', 'DocumentsRequireComplete']) expect(refs).toContain(`#/components/parameters/${ref}`)
  })
})

describe('anchors (§3)', () => {
  it('accept the fixtures, and a find anchor quotes exactly its UTF-8 range of the block', () => {
    const page = fixture('text-page.json')
    const found = fixture('find.json')
    ok('DocumentsTextPage', page)
    ok('DocumentsFindResult', found)
    const a = found.matches[0]
    const block = page.blocks.find((b: any) => b.block_id === a.block_id)
    const bytes = new TextEncoder().encode(block.text)
    expect(new TextDecoder().decode(bytes.slice(a.text_range.start, a.text_range.end))).toBe(a.quote)
    expect(a.block_text_sha256).toBe(block.text_sha256)
    expect(a.text_layer_sha256).toBe(page.text_layer_sha256)
  })

  it('pin the frame, the unit and the shape of every field', () => {
    const a = fixture('find.json').matches[0]
    const bad: Array<[string, (x: any) => void]> = [
      ['utf16 offsets', (x) => { x.text_range.unit = 'utf16' }],
      ['no quote', (x) => { delete x.quote }],
      ['no text layer', (x) => { delete x.text_layer_sha256 }],
      ['no block hash', (x) => { delete x.block_text_sha256 }],
      ['MediaBox frame', (x) => { x.geometry.box = 'MediaBox' }],
      ['bottom-left origin', (x) => { x.geometry.origin = 'bottom-left' }],
      ['three-number rect', (x) => { x.geometry.rects = [[1, 2, 3]] }],
      ['negative coordinate', (x) => { x.geometry.rects = [[-1, 2, 3, 4]] }],
      ['no rects', (x) => { x.geometry.rects = [] }],
      ['points not stated', (x) => { x.geometry.unit = 'px' }],
      ['no engine', (x) => { delete x.engine }],
      ['no range start', (x) => { delete x.text_range.start }],
      ['page without geometry', (x) => { delete x.geometry }],
      ['geometry without page', (x) => { delete x.page }],
      ['page 0', (x) => { x.page = 0 }],
      ['revision 0', (x) => { x.revision = 0 }],
    ]
    for (const [name, mutate] of bad) {
      const copy = clone(a)
      mutate(copy)
      expect(v.DocumentsAnchor(copy), name).toBe(false)
    }
  })

  it('a paginated anchor always has a page and geometry; a flowing one has neither', () => {
    const a = fixture('find.json').matches[0]
    const { page: _page, geometry: _geometry, ...located } = clone(a)
    expect(v.DocumentsAnchor(located), 'PDF anchor without page and geometry').toBe(false)
    for (const media_type of ['image/jpeg', 'image/png']) {
      expect(v.DocumentsAnchor({ ...located, media_type }), media_type).toBe(false)
    }
    const docx = { ...located, media_type: DOCX, engine: { id: 'superdoc', version: '2.18.0' }, block_id: 'body/p12' }
    ok('DocumentsAnchor', docx)
    expect(v.DocumentsAnchor({ ...docx, page: 1 }), 'flowing anchor with a page only').toBe(false)
    expect(v.DocumentsAnchor({ ...docx, geometry: a.geometry }), 'flowing anchor with geometry only').toBe(false)
    expect(v.DocumentsAnchor({ ...docx, page: 1, geometry: a.geometry }), 'flowing anchor with both').toBe(false)
    expect(v.DocumentsAnchor({ ...a, media_type: '' }), 'empty media type').toBe(false)
    expect(v.DocumentsAnchor({ ...located, media_type: 'pdf' }), 'not a type/subtype').toBe(false)
    const { media_type: _m, ...untyped } = clone(a)
    expect(v.DocumentsAnchor(untyped), 'no media type').toBe(false)
  })

  it('a partial parse lists what was not parsed, in text pages and find results', () => {
    const regions = [{ page: 2, reason: 'scanned page without OCR' }]
    for (const [schema, base] of [['DocumentsTextPage', fixture('text-page.json')], ['DocumentsFindResult', fixture('find.json')]] as const) {
      const partial = { ...base, parse_status: 'partial' }
      expect(v[schema](partial), `${schema} without regions`).toBe(false)
      expect(v[schema]({ ...partial, unparsed_regions: [] }), `${schema} with no regions`).toBe(false)
      ok(schema, { ...partial, unparsed_regions: regions })
      // complete means everything was parsed: no regions left out (#817 review)
      expect(v[schema]({ ...base, parse_status: 'complete', unparsed_regions: regions }), `${schema} complete with regions`).toBe(false)
      ok(schema, { ...base, parse_status: 'complete', unparsed_regions: [] })
    }
  })

  it('define the paginated formats and the media type syntax once (#817 review)', () => {
    const s = openapi.components.schemas
    expect(s.DocumentsPaginatedMediaType.enum).toEqual(['application/pdf', 'image/jpeg', 'image/png'])
    const paginated = { $ref: '#/components/schemas/DocumentsPaginatedMediaType' }
    expect(s.DocumentsAnchor.if.properties.media_type).toEqual(paginated)
    expect(s.DocumentsTextPage.allOf[1].if.properties.media_type).toEqual(paginated)
    const mediaType = '#/components/schemas/DocumentsMediaType'
    for (const schema of ['DocumentsAnchor', 'DocumentsTextPage', 'Document', 'DocumentRevisionSummary']) {
      expect(s[schema].properties.media_type.$ref, schema).toBe(mediaType)
    }
    const syntax = new RegExp(s.DocumentsMediaType.pattern)
    expect(syntax.test('application/pdf')).toBe(true)
    for (const bad of ['Application/PDF', 'application/pdf; charset=x', 'pdf', '']) expect(syntax.test(bad), bad).toBe(false)
    // ADR §3 agrees: a flowing format gives no page at all.
    const adr = readFileSync(join(repoRoot, 'docs', 'documenting', 'adr', 'ADR-DOC-02-operation-contract.md'), 'utf8')
    expect(adr).toMatch(/DOCX 等流式格式只用它定位，不给 `page`/)
    expect(adr).not.toMatch(/`page` 可省略/)
  })

  it('a text block of a paginated source has a page and geometry; a flowing block has neither', () => {
    const page = fixture('text-page.json')
    const { geometry: _g, ...noGeometry } = page.blocks[0]
    expect(v.DocumentsTextPage({ ...page, blocks: [noGeometry] })).toBe(false)
    const { page: _p, ...flowing } = noGeometry
    expect(v.DocumentsTextPage({ ...page, blocks: [flowing] }), 'PDF block without page and geometry').toBe(false)
    ok('DocumentsTextPage', { ...page, media_type: DOCX, engine: { id: 'superdoc', version: '2.18.0' }, blocks: [flowing] })
    const docxPage = { ...page, media_type: DOCX, engine: { id: 'superdoc', version: '2.18.0' } }
    expect(v.DocumentsTextPage({ ...docxPage, blocks: [page.blocks[0]] }), 'flowing block with page and geometry').toBe(false)
    for (const bad of ['', 'pdf']) {
      expect(v.DocumentsTextPage({ ...docxPage, media_type: bad, blocks: [flowing] }), `media type ${JSON.stringify(bad)}`).toBe(false)
    }
  })
})

describe('read requests and errors', () => {
  it('find takes a revision and a bounded query, nothing else', () => {
    ok('DocumentsFindRequest', { revision: 1, query: '汇算' })
    ok('DocumentsFindRequest', { revision: 1, query: '汇算', cursor: 'c1', limit: 200, require_complete: true })
    const tooLong = 'x'.repeat(501)
    for (const body of [{ revision: 1, query: '' }, { query: 'x' }, { revision: 1, query: 'x', limit: 201 }, { revision: 1, query: 'x', limit: 0 },
      { revision: 1, query: tooLong }, { revision: 1, query: 'x', require_complete: 'yes' }, { revision: 1, query: 'x', path: '/tmp/a' }]) {
      expect(v.DocumentsFindRequest(body), JSON.stringify(body)).toBe(false)
    }
  })

  it('a text or find 422 admits exactly its three codes', () => {
    const all = openapi.components.schemas.DocumentsErrorCode.enum as string[]
    const expected: Record<string, string[]> = {
      DocumentsReadUnprocessable: ['unsupported_format', 'parse_failed', 'partial_parse'],
    }
    for (const [response, codes] of Object.entries(expected)) {
      for (const code of all) expect(v[response](err(code)), `${response} ${code}`).toBe(codes.includes(code))
    }
  })
})
