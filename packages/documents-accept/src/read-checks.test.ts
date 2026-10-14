import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { MAX_PAGES } from './checks'
import { checkFind, checkRender, checkText, locator } from './read-checks'
import { type Sample, loadSample } from './samples'
import { type Call, anchor, answer, block, fakeClient, found, id, refuse, sha, textPage } from './test/fake'

const S01 = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..', 'docs', 'documenting', 'samples', 's01')
const NOTICE = loadSample(path.join(S01, 's01-07-mixed-building-notice')) // one page
const FEES = loadSample(path.join(S01, 's01-01-zh-sh-fee-2025')) // 33 pages
const DOC = id('doc')
const cursorOf = (call: Call) => new URLSearchParams(call.route.split('?')[1]).get('cursor')
const NOT_A = (schema: string) => expect.stringContaining(`the answer is not a ${schema}`)

describe('ACC-TEXT', () => {
  const TEXT = `/documents/${DOC}/revisions/1/text`

  it('reads the text to its end, and records what it saw', async () => {
    const unread = { page: 2, reason: 'ocr_failed' }
    const { client, calls } = fakeClient((call) => {
      const at = cursorOf(call)
      if (at === null) return answer(textPage([block('p1/b1'), block('p1/b2')], 'c1'))
      if (at === 'c1') return answer(textPage([], 'c 2', { parse_status: 'partial', unparsed_regions: [unread] }))
      return answer(textPage([block('p3/b1', 3)], null))
    })
    expect(await checkText(client, FEES, DOC)).toMatchObject({
      id: 'ACC-TEXT',
      outcome: 'passed',
      text: { textLayer: sha('a'), blocks: 3, partial: true, unparsed: [unread] },
    })
    expect(calls.map((c) => c.route)).toEqual([TEXT, `${TEXT}?cursor=c1`, `${TEXT}?cursor=c%202`])
  })

  it('fails when the text never ends, changes layer on the way, or has a block past the last page', async () => {
    const problems = async (daemon: (call: Call) => Response, sample: Sample = FEES) => {
      const result = await checkText(fakeClient(daemon).client, sample, DOC)
      expect(result.text).toBeUndefined()
      return result.problems
    }
    expect(await problems(() => answer(textPage([], 'again')))).toEqual(['the pages do not end: cursor again came twice'])
    let n = 0
    expect(await problems(() => answer(textPage([], String((n += 1)))))).toEqual([`the pages do not end: more than ${MAX_PAGES}`])
    expect(await problems((call) => answer(cursorOf(call) ? textPage([], null, { text_layer_sha256: sha('c') }) : textPage([], 'c1')))).toEqual(['the text layer changed between two pages of one reading'])
    expect(await problems(() => answer(textPage([block('p2/b1', 2)], null)), NOTICE)).toEqual(['block p2/b1 is on page 2 of a document of 1'])
    expect(await problems(() => refuse(422, 'parse_failed'))).toEqual(['422 parse_failed: it is parse_failed'])
    expect(await problems(() => answer(textPage([], null, { document_id: id('doc', 2) })))).toEqual([`asked for the text of ${DOC}, answered with ${id('doc', 2)}`])
    expect(await problems(() => answer(textPage([], null, { revision: 2 })))).toEqual(['asked for the text of revision 1, answered with revision 2'])
    expect(await problems((call) => answer(cursorOf(call) ? textPage([], null, { revision: 2 }) : textPage([], 'c1')))).toEqual(['asked for the text of revision 1, answered with revision 2'])
  })

  it('fails on a page that comes with another status than 200', async () => {
    expect((await checkText(fakeClient(() => answer(textPage([], null), 206)).client, FEES, DOC)).problems).toEqual([`GET ${TEXT} answered 206, not 200`])
    expect((await checkFind(fakeClient(() => answer(found([anchor(1, 'p1/b1')]), 201)).client, NOTICE, DOC)).problems).toEqual([`POST /documents/${DOC}/find answered 201, not 200`])
  })

  it('fails on a page that is not what the contract says', async () => {
    const problems = async (page: object) => (await checkText(fakeClient(() => answer(page)).client, FEES, DOC)).problems
    expect(await problems(textPage([], null, { text_layer_sha256: undefined }))).toEqual([NOT_A('DocumentsTextPage')])
    expect(await problems(textPage([], null, { parse_status: 'done' }))).toEqual([NOT_A('DocumentsTextPage')])
    // Partial without saying which regions were not read, or complete with some.
    expect(await problems(textPage([], null, { parse_status: 'partial' }))).toEqual([NOT_A('DocumentsTextPage')])
    expect(await problems(textPage([], null, { unparsed_regions: [{ reason: 'ocr_failed' }] }))).toEqual([NOT_A('DocumentsTextPage')])
    expect(await problems(textPage([{ block_id: 'p1/b1', text: 'x' }], null))).toEqual([NOT_A('DocumentsTextPage')])
    expect(await problems({ blocks: [], next_cursor: null })).toEqual([NOT_A('DocumentsTextPage')])
  })
})

describe('ACC-RENDER', () => {
  // A PNG as far as its header chunk: the signature, then IHDR with a width and a height.
  const SIGNATURE = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]
  const IHDR = [0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52]
  const PICTURE = [...SIGNATURE, ...IHDR, 0, 0, 2, 100, 0, 0, 3, 24]
  const png = (more: { bytes?: number[]; type?: string; scale?: string | null; status?: number } = {}) => {
    const headers: Record<string, string> = { 'content-type': more.type ?? 'image/png' }
    if (more.scale !== null) headers['documents-render-scale'] = more.scale ?? '1'
    return new Response(new Uint8Array(more.bytes ?? PICTURE), { status: more.status ?? 200, headers })
  }
  const pageOf = (call: Call) => Number(call.route.match(/\/pages\/(\d+)\?scale=1$/)?.[1])
  const daemon = (pages: number, first = png()) => (call: Call) => (pageOf(call) > pages ? refuse(404, 'not_found') : pageOf(call) === 1 ? first : png())
  const run = async (sample: Sample, d: (call: Call) => Response) => {
    const { client, calls } = fakeClient(d)
    return { result: await checkRender(client, sample, DOC), pages: calls.map(pageOf) }
  }

  it('renders the first page and the last, and finds the one after the last missing', async () => {
    expect(await run(FEES, daemon(33))).toMatchObject({ result: { id: 'ACC-RENDER', outcome: 'passed', problems: [] }, pages: [1, 33, 34] })
    // A document of one page is rendered once.
    expect((await run(NOTICE, daemon(1))).pages).toEqual([1, 2])
  })

  const problems = async (first: Response) => (await run(NOTICE, daemon(1, first))).result.problems

  it('fails on a page that is not a PNG', async () => {
    const NOT_PNG = ['page 1 is not a PNG']
    expect(await problems(png({ bytes: [0xff, 0xd8, 0xff] }))).toEqual(NOT_PNG)
    // The signature alone, with no header chunk, with another chunk first, or with no size.
    expect(await problems(png({ bytes: SIGNATURE }))).toEqual(NOT_PNG)
    expect(await problems(png({ bytes: PICTURE.slice(0, 23) }))).toEqual(NOT_PNG)
    expect(await problems(png({ bytes: [...SIGNATURE, 0, 0, 0, 13, 0x49, 0x44, 0x41, 0x54, ...PICTURE.slice(16)] }))).toEqual(NOT_PNG)
    expect(await problems(png({ bytes: [...SIGNATURE, ...IHDR, 0, 0, 0, 0, 0, 0, 3, 24] }))).toEqual(NOT_PNG)
    expect(await problems(png({ bytes: [...SIGNATURE, ...IHDR, 0, 0, 2, 100, 0, 0, 0, 0] }))).toEqual(NOT_PNG)
    expect(await problems(png({ type: 'application/json' }))).toEqual(NOT_PNG)
    expect(await problems(png({ type: 'image/pngx' }))).toEqual(NOT_PNG)
    expect(await problems(png({ type: 'image/png; q=1' }))).toEqual([])
    expect(await problems(png({ status: 206 }))).toEqual(['page 1 was answered with 206, not 200'])
  })

  it('fails on a page that does not say its scale, or was rendered above the scale asked for', async () => {
    const UNSAID = ['page 1 does not say the scale it was rendered at']
    for (const scale of [null, '', '9', '0.2', '0x1', 'one', '1,5', '-1']) expect(await problems(png({ scale })), String(scale)).toEqual(UNSAID)
    for (const scale of ['2', '4', '1.01']) expect(await problems(png({ scale }))).toEqual([`page 1 was rendered at scale ${scale}, above the 1 asked for`])
    for (const scale of ['0.25', '0.5', '1', '1.0', '1e0']) expect(await problems(png({ scale })), scale).toEqual([])
  })

  it('fails when the page past the last is rendered, or refused for another reason', async () => {
    expect((await run(NOTICE, daemon(5))).result.problems).toEqual(['page 2, past the last, was rendered'])
    const refusing = (status: number, code: string) => (call: Call) => (pageOf(call) > 1 ? refuse(status, code) : png())
    expect((await run(NOTICE, refusing(404, 'module_not_ready'))).result.problems).toEqual(['404 module_not_ready: it is module_not_ready'])
    expect((await run(NOTICE, refusing(422, 'not_found'))).result.problems).toEqual(['422 not_found: it is not_found'])
  })
})

describe('ACC-FIND', () => {
  const anchors = NOTICE.gold.fields.flatMap((f) => [...(f.anchors ?? []), ...(f.candidates ?? []).flatMap((c) => c.anchors)])
  const queries = new Set(anchors.map((a) => a.quote))
  const lost = '工单号 Work Order: WO-000731'

  it('looks up every gold quote once, and counts the anchors find reached on their page', async () => {
    const { client, calls } = fakeClient((call) => {
      const { query } = call.json as { query: string }
      return answer(found(query === lost ? [anchor(2, 'p2/b1')] : [anchor(1, 'p1/b1'), anchor(3, 'p3/b9')]))
    })
    const result = await checkFind(client, NOTICE, DOC)
    expect(result).toMatchObject({ id: 'ACC-FIND', outcome: 'passed', problems: [] })
    expect(calls.map((c) => c.json)).toEqual([...queries].map((query) => ({ revision: 1, query })))
    expect(anchors.length).toBeGreaterThan(queries.size)
    expect(result.find).toMatchObject({ anchors: anchors.length, reachable: anchors.length - 1 })
    // Only the matches on the anchor's own page are where it is.
    const locate = locator(result.find!.located)
    const another = anchors.find((a) => a.quote !== lost)!
    expect(locate({ page: 1, quote: lost })).toEqual([])
    expect(locate(another)).toEqual([anchor(1, 'p1/b1')])
    expect(locate({ page: 1, quote: 'a quote that was never looked up' })).toEqual([])
    // The same words asked for on another page are another anchor.
    expect(locate({ page: 2, quote: another.quote })).toEqual([])
  })

  it('records the regions find did not search, with the quote it looked up there', async () => {
    const unread = { page: 1, reason: 'ocr_failed' }
    const { client } = fakeClient((call) => {
      const { query } = call.json as { query: string }
      return answer(query === lost ? { ...found([]), parse_status: 'partial', unparsed_regions: [unread] } : found([anchor(1, 'p1/b1')]))
    })
    const { located } = (await checkFind(client, NOTICE, DOC)).find!
    expect(located.find((l) => l.quote === lost)).toEqual({ page: 1, quote: lost, found: [], unparsed: [unread] })
    expect(located.filter((l) => l.unparsed !== undefined)).toHaveLength(1)
  })

  it('reads every page of matches', async () => {
    const { client, calls } = fakeClient((call) => {
      const { cursor } = call.json as { cursor?: string }
      return answer(found([anchor(1, `p1/b${cursor ?? 0}`)], cursor === undefined ? '1' : cursor === '1' ? '2' : null))
    })
    const result = await checkFind(client, NOTICE, DOC)
    expect(calls).toHaveLength(queries.size * 3)
    expect(result.find?.located[0]?.found.map((m) => m.block_id)).toEqual(['p1/b0', 'p1/b1', 'p1/b2'])
  })

  it('fails, and does not go on for ever, when the pages of matches do not end', async () => {
    const turning = fakeClient((call) => answer(found([], (call.json as { cursor?: string }).cursor === 'a' ? 'b' : 'a')))
    expect((await checkFind(turning.client, NOTICE, DOC)).problems).toEqual(['the pages do not end: cursor a came twice'])
  })

  it('records a quote find will not take as a query, and goes on with the others', async () => {
    const { client, calls } = fakeClient((call) => ((call.json as { query: string }).query === lost ? refuse(400, 'invalid_request') : answer(found([anchor(1, 'p1/b1')]))))
    const result = await checkFind(client, NOTICE, DOC)
    expect(result.outcome).toBe('passed')
    expect(calls).toHaveLength(queries.size)
    expect(result.find?.located.find((l) => l.quote === lost)).toEqual({ page: 1, quote: lost, found: [], refused: 'invalid_request' })
    expect(result.find).toMatchObject({ anchors: anchors.length, reachable: anchors.length - 1 })
  })

  it('fails when a later page of matches is refused: the quote was a query, and its matches are not to be lost', async () => {
    const { client } = fakeClient((call) => {
      const { query, cursor } = call.json as { query: string; cursor?: string }
      if (query !== lost) return answer(found([anchor(1, 'p1/b1')]))
      return cursor === undefined ? answer(found([anchor(1, 'p1/b2')], 'more')) : refuse(400, 'invalid_request')
    })
    const result = await checkFind(client, NOTICE, DOC)
    expect(result).toMatchObject({ outcome: 'failed', problems: ['400 invalid_request: it is invalid_request'] })
    expect(result.find).toBeUndefined()
  })

  it('fails when find takes no quote at all: then it is the request that is wrong', async () => {
    const result = await checkFind(fakeClient(() => refuse(400, 'invalid_request')).client, NOTICE, DOC)
    expect(result).toMatchObject({ outcome: 'failed', problems: ['find refused every quote as a query (invalid_request)'] })
    expect(result.find).toBeUndefined()
  })

  it('fails when find itself does, or answers with what the contract does not have', async () => {
    const result = await checkFind(fakeClient(() => refuse(503, 'engine_unavailable')).client, NOTICE, DOC)
    expect(result).toMatchObject({ outcome: 'failed', problems: ['503 engine_unavailable: it is engine_unavailable'] })
    expect(result.find).toBeUndefined()
    const problems = async (body: object) => (await checkFind(fakeClient(() => answer(body)).client, NOTICE, DOC)).problems
    expect(await problems(found([anchor(1, 'p1/b1', 0, 4, { document_id: id('doc', 2) })]))).toEqual([`find answered ${DOC} with matches of another document`])
    // Of another revision, on the anchor's own page or on any other.
    expect(await problems(found([anchor(1, 'p1/b1', 0, 4, { revision: 2 })]))).toEqual(['find answered revision 1 with matches of another revision'])
    expect(await problems(found([anchor(1, 'p1/b1'), anchor(9, 'p9/b1', 0, 4, { revision: 2 })]))).toEqual(['find answered revision 1 with matches of another revision'])
    expect(await problems({ matches: [], next_cursor: null })).toEqual([NOT_A('DocumentsFindResult')])
    expect(await problems(found([{ page: 1, block_id: 'p1/b1' }]))).toEqual([NOT_A('DocumentsFindResult')])
    expect(await problems(found([anchor(1, 'p1/b1', 0, 4, { text_range: { unit: 'utf16', start: 0, end: 4 } })]))).toEqual([NOT_A('DocumentsFindResult')])
  })
})
