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
  /** An Office document read as the PDF it was converted to: sources are that PDF's pages. */
  converted?: boolean
  rows: ExtractRow[]
}

/** A field's type hint for the extractor; values keep their wording either way. */
function typeOf(name: string): 'date' | 'amount' | 'text' {
  if (/日期|时间|截止/.test(name)) return 'date'
  if (/金额|价格|费用|总价|总额|租金|押金/.test(name)) return 'amount'
  return 'text'
}

/** A failure: said in plain Chinese; what the OS answered kept for 「详情」. */
export interface ExtractFailure { error: string; detail?: string }
type Failed = Parameters<typeof describeError>[0]

function failed(res: Failed): ExtractFailure {
  const detail = describeError(res)
  if (res.error.code === 'engine_unavailable') {
    return res.error.details?.['engine'] === 'local-model'
      ? { error: '本地模型没有分配给文档模块，暂时不能抽取。', detail }
      : { error: '这台机器上读不出这份文档的文字，暂时不能抽取。', detail }
  }
  // Delegation and presence answers already say, in the user's words, what to do (#2019 Low).
  if (/^(module_delegation|presence)/.test(res.error.code)) return { error: detail }
  return { error: '文档模块没有完成这次抽取。', detail }
}

/** Extracts `names` from the head revision of `documentId`: the card, or what went wrong. */
export async function extractFields(
  api: Api,
  documentId: string,
  names: readonly string[],
  sleep: Sleep = (ms) => new Promise((r) => setTimeout(r, ms)),
): Promise<ExtractCardData | ExtractFailure> {
  const got = await api.request({ op: 'get', documentId })
  if (!got.ok) return failed(got)
  let docId = got.data.document_id
  let revision = got.data.head.revision
  const title = got.data.title ?? ''
  if (OFFICE.test(got.data.head.media_type)) {
    const pdf = await convertedPdf(api, docId, title)
    if (!pdf.ok || !pdf.documentId) {
      return pdf.ok ? { error: '没能转换成 PDF。' }
        : pdf.engineMissing ? { error: '需要先在文档页下载转换器（约 340 MB，只需一次）。' }
          : { error: '没能把这份文档转换成 PDF，暂时不能抽取。', detail: pdf.message }
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
  if (!res.ok) return failed(res)
  let job = res.data
  // A long document is many model calls: wait up to 15 minutes.
  for (let i = 0; i < 600 && (job.status === 'queued' || job.status === 'running'); i++) {
    await sleep(1500)
    const next = await api.request({ op: 'job', jobId: job.job_id })
    if (!next.ok) return failed(next)
    job = next.data
  }
  const extractionId = job.result?.extraction_id
  if (job.status === 'queued' || job.status === 'running') {
    return { error: '抽取还在进行（长文档要更久）。稍后再说一次同样的话，会直接用这次的结果。', detail: job.job_id }
  }
  if (job.status !== 'succeeded' || !extractionId) {
    const detail = job.error ? `${job.error.message}（${job.error.code}）` : job.status
    return { error: job.status === 'cancelled' ? '抽取已取消。' : '本地模型没有完成这次抽取，请稍后再试。', detail }
  }
  const values: Value[] = []
  let cursor: string | undefined
  for (let i = 0; i < 20; i++) {
    const page = await api.request({ op: 'getExtraction', extractionId, ...(cursor ? { cursor } : {}) })
    if (!page.ok) return failed(page)
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
  return { documentId: docId, title, ...(docId !== got.data.document_id ? { converted: true } : {}), rows }
}

/** The card as plain text: what the conversation history keeps. */
export function extractText(card: ExtractCardData): string {
  const lines = card.rows.map((r) => {
    if (r.values.length === 0) return `- ${r.name}：文档里没找到`
    const pages = [...new Set(r.sources.flatMap((s) => (s.page === undefined ? [] : [s.page])))]
    const where = pages.length ? `（${card.converted ? '转换后' : ''}第 ${pages.join('、')} 页）` : ''
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
