// F3 一句话抽取 (Loop 6): the fields a user names, extracted by the documents
// OS's extract job (its local model reads the document, every value carries
// the block it was read from), never answered by the chat model itself. An
// Office document has no text of its own: it is extracted as the PDF it
// becomes, so a value's source is a page of that PDF.

import type { components } from '@agent24/api-client'
import { describeError } from './importStore'
import { convertedPdf } from './compose'
import type { Source } from './doc-check'

type Api = Window['agent24']['documents']
type Sleep = (ms: number) => Promise<unknown>
type Anchor = components['schemas']['DocumentsAnchor']
type Value = components['schemas']['DocumentsExtractedValue']
const OFFICE = /officedocument/

export interface ExtractRow {
  name: string
  /** The value as written; several when the document states different ones. */
  values: string[]
  sources: Source[]
}
export interface ExtractCardData {
  /** The document the sources are in (an Office document's PDF). */
  documentId: string
  title: string
  rows: ExtractRow[]
}

/** A field's type hint for the extractor; values keep their wording either way. */
function typeOf(name: string): 'date' | 'amount' | 'text' {
  if (/日期|时间|截止/.test(name)) return 'date'
  if (/金额|价格|费用|总价|总额|租金|押金/.test(name)) return 'amount'
  return 'text'
}

/** Extracts `names` from the head revision of `documentId`: the card, or what went wrong. */
export async function extractFields(
  api: Api,
  documentId: string,
  names: readonly string[],
  sleep: Sleep = (ms) => new Promise((r) => setTimeout(r, ms)),
): Promise<ExtractCardData | { error: string }> {
  const got = await api.request({ op: 'get', documentId })
  if (!got.ok) return { error: describeError(got) }
  let docId = got.data.document_id
  let revision = got.data.head.revision
  const title = got.data.title ?? ''
  if (OFFICE.test(got.data.head.media_type)) {
    const pdf = await convertedPdf(api, docId, title)
    if (!pdf.ok || !pdf.documentId) {
      return { error: pdf.ok ? '没能转换成 PDF。' : pdf.engineMissing ? '需要先在文档页下载转换器（约 340 MB，只需一次）。' : pdf.message }
    }
    docId = pdf.documentId
    revision = 1
  }
  const fields = names.map((n, i) => ({ key: `f${i + 1}`, description: n, type: typeOf(n) }))
  const submit = (rerun: boolean) => api.request({
    op: 'extract',
    documentId: docId,
    revision,
    schema: { fields },
    ...(rerun ? { rerun: true, idempotencyKey: `f3-${Date.now()}` } : {}),
  })
  let res = await submit(false)
  // The same fields asked before replay that job: one that failed is run again.
  if (res.ok && (res.data.status === 'failed' || res.data.status === 'cancelled' || res.data.status === 'interrupted')) res = await submit(true)
  if (!res.ok) return { error: describeError(res) }
  let job = res.data
  // A long document is many model calls: wait up to 15 minutes.
  for (let i = 0; i < 600 && (job.status === 'queued' || job.status === 'running'); i++) {
    await sleep(1500)
    const next = await api.request({ op: 'job', jobId: job.job_id })
    if (!next.ok) return { error: describeError(next) }
    job = next.data
  }
  const extractionId = job.result?.extraction_id
  if (job.status !== 'succeeded' || !extractionId) return { error: job.error?.message ?? `抽取没有完成（${job.status}）` }
  const values: Value[] = []
  let cursor: string | undefined
  for (let i = 0; i < 20; i++) {
    const page = await api.request({ op: 'getExtraction', extractionId, ...(cursor ? { cursor } : {}) })
    if (!page.ok) return { error: describeError(page) }
    values.push(...page.data.values)
    if (!page.data.next_cursor) break
    cursor = page.data.next_cursor
  }
  const rows = fields.map((f): ExtractRow => {
    const v = values.find((x) => x.key === f.key)
    const src = (anchors?: readonly Anchor[]): Source[] => (anchors ?? []).map((a) => ({ block_id: a.block_id, page: a.page, quote: a.quote }))
    if (v?.status === 'present') return { name: f.description, values: [v.value ?? ''], sources: src(v.anchors) }
    if (v?.status === 'conflict') {
      const c = v.candidates ?? []
      return { name: f.description, values: c.map((x) => x.value), sources: c.flatMap((x) => src(x.anchors)) }
    }
    return { name: f.description, values: [], sources: [] }
  })
  return { documentId: docId, title, rows }
}

/** The card as plain text: what the conversation history keeps. */
export function extractText(card: ExtractCardData): string {
  const lines = card.rows.map((r) => {
    if (r.values.length === 0) return `- ${r.name}：文档里没找到`
    const pages = [...new Set(r.sources.flatMap((s) => (s.page === undefined ? [] : [s.page])))]
    const where = pages.length ? `（第 ${pages.join('、')} 页）` : ''
    return `- ${r.name}：${r.values.join(' / ')}${r.values.length > 1 ? '（文档里写法不一致）' : ''}${where}`
  })
  return [`从《${card.title}》抽取：`, ...lines].join('\n')
}

/** The fallback when the rules read no field name from the sentence: the
 * model only lists the names; it never reads the document or answers. */
export async function fieldNamesFromModel(ask: (prompt: string) => Promise<string>, sentence: string): Promise<string[]> {
  const out = await ask(`用户想从一份文档里抽取一些字段。只列出下面这句话要的字段名，每行一个，不要解释，不要回答内容。\n这句话：${sentence}`)
  const names = out.replace(/<think>[\s\S]*?<\/think>/g, '').split(/[\n、，,]/)
    .map((s) => s.replace(/^[\s\-*•\d.)、]+/, '').trim())
    .filter((s) => s.length > 0 && s.length <= 30)
  return [...new Set(names)].slice(0, 20)
}
