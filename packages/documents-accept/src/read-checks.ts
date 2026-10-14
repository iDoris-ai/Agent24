// The checks that read a stored document back (acceptance README §3): its
// text layer, its pages as pictures, and where `find` puts each gold quote.

import type { components } from '@agent24/api-client'
import type { Anchor, Locate } from './anchors'
import { type CheckResult, check, pages } from './checks'
import { ApiError, type Client } from './client'
import type { GoldAnchor, Sample } from './samples'

type Schemas = components['schemas']

/** What reading the whole text layer showed. Recorded, not judged (§3). */
export interface TextRead {
  textLayer: string
  blocks: number
  /** Whether any page had regions that were not read, and those regions. */
  partial: boolean
  unparsed: Schemas['DocumentsUnparsedRegion'][]
}

/** ACC-TEXT: the text of revision 1 can be read to its end. */
export async function checkText(client: Client, sample: Sample, documentId: string): Promise<CheckResult & { text?: TextRead }> {
  let text: TextRead | undefined
  const result = await check('ACC-TEXT', sample.id, async () => {
    const read: TextRead = { textLayer: '', blocks: 0, partial: false, unparsed: [] }
    const route = `/documents/${documentId}/revisions/1/text`
    for await (const page of pages((cursor) => client.json<Schemas['DocumentsTextPage']>('GET', cursor ? `${route}?cursor=${encodeURIComponent(cursor)}` : route, 'DocumentsTextPage'))) {
      if (page.document_id !== documentId) return [`asked for the text of ${documentId}, answered with ${page.document_id}`]
      if (read.textLayer !== '' && page.text_layer_sha256 !== read.textLayer) return ['the text layer changed between two pages of one reading']
      read.textLayer = page.text_layer_sha256
      read.blocks += page.blocks.length
      read.partial ||= page.parse_status === 'partial'
      read.unparsed.push(...(page.unparsed_regions ?? []))
      const beyond = page.blocks.find((b) => (b.page ?? 1) > sample.meta.pages)
      if (beyond) return [`block ${beyond.block_id} is on page ${beyond.page} of a document of ${sample.meta.pages}`]
    }
    text = read
    return []
  })
  return { ...result, text }
}

const PNG = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]

/** ACC-RENDER: the first and the last page come back as PNG, with the scale
 *  that was used; the page after the last is not found. */
export async function checkRender(client: Client, sample: Sample, documentId: string): Promise<CheckResult> {
  const route = (page: number) => `/documents/${documentId}/revisions/1/pages/${page}?scale=1`
  return check('ACC-RENDER', sample.id, async () => {
    const problems: string[] = []
    for (const page of new Set([1, sample.meta.pages])) {
      const response = await client.send('GET', route(page))
      const bytes = new Uint8Array(await response.arrayBuffer())
      if (!response.headers.get('content-type')?.startsWith('image/png') || !PNG.every((b, i) => bytes[i] === b)) problems.push(`page ${page} is not a PNG`)
      const scale = Number(response.headers.get('documents-render-scale'))
      if (!(scale >= 0.25 && scale <= 4)) problems.push(`page ${page} does not say the scale it was rendered at`)
    }
    try {
      await client.send('GET', route(sample.meta.pages + 1))
      problems.push(`page ${sample.meta.pages + 1}, past the last, was rendered`)
    } catch (error) {
      if (!(error instanceof ApiError && error.status === 404 && error.code === 'not_found')) throw error
    }
    return problems
  })
}

/** A gold anchor's quote, and where `find` put it on the anchor's page. */
export interface Located {
  page: number
  quote: string
  found: Anchor[]
  /** The error code, when `find` would not take this quote as a query. */
  refused?: string
}

export interface FindRead {
  located: Located[]
  /** Gold anchors, and how many of them `find` reached on their page (§4.4). */
  anchors: number
  reachable: number
}

/** ACC-FIND: every gold quote is looked up once. Whether it is found is
 *  recorded, not judged: it measures the read engine and the matching rule.
 *  A quote `find` refuses as a query (a 400 to the first request for it) is
 *  recorded as not found; a 400 to a later page of its matches is a failure. */
export async function checkFind(client: Client, sample: Sample, documentId: string): Promise<CheckResult & { find?: FindRead }> {
  let find: FindRead | undefined
  const result = await check('ACC-FIND', sample.id, async () => {
    const anchors = sample.gold.fields.flatMap((f) => [...(f.anchors ?? []), ...(f.candidates ?? []).flatMap((c) => c.anchors)])
    const located: Located[] = []
    for (const { page, quote } of anchors) {
      if (located.some((l) => l.page === page && l.quote === quote)) continue
      const found: Anchor[] = []
      let asked = 0
      const ask = (cursor: string) => {
        asked += 1
        return client.json<Schemas['DocumentsFindResult']>('POST', `/documents/${documentId}/find`, 'DocumentsFindResult', { json: { revision: 1, query: quote, ...(cursor ? { cursor } : {}) } })
      }
      try {
        for await (const hits of pages(ask)) {
          if (hits.matches.some((m) => m.document_id !== documentId)) throw new Error(`find answered ${documentId} with matches of another document`)
          found.push(...hits.matches.filter((m) => m.page === page))
        }
        located.push({ page, quote, found })
      } catch (error) {
        if (!(error instanceof ApiError && error.status === 400 && asked === 1)) throw error
        located.push({ page, quote, found: [], refused: error.code })
      }
    }
    // One quote may be no query; when none is, it is the request that is wrong.
    if (located.length > 0 && located.every((l) => l.refused !== undefined)) return [`find refused every quote as a query (${located[0]?.refused})`]
    const locate = locator(located)
    find = { located, anchors: anchors.length, reachable: anchors.filter((a) => locate(a).length > 0).length }
    return []
  })
  return { ...result, find }
}

/** What the anchor scoring asks: where `find` put this gold anchor (§4.4). */
export function locator(located: Located[]): Locate {
  return (anchor: GoldAnchor) => located.find((l) => l.page === anchor.page && l.quote === anchor.quote)?.found ?? []
}
