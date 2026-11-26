// L8 DOC-4 (first slice): 「这份合同要发给对方了，帮我检查一下有没有没删干净的东西」
// 「去掉修订和批注，导出一份 PDF 放桌面」「我上次发出去的是哪个版本？」.
// As with compare and extract (F3, S05), the App judges the user's own words
// and asks the module; a small local model does not call tools reliably. The
// check only reads; a clean copy is made only from the card's button, after
// the user has seen what is in the document, and is saved where the user
// picks in the system save dialog (main `doc-deliver.ts`). Nothing here is
// the model's.

import type { DeliveryReceipt, DocumentPreflight } from '../../../shared/ipc-types'

type Api = Window['agent24']['documents']

export type DeliverAsk =
  /** Check what the document still carries (read-only). */
  | { kind: 'check' }
  /** Make a clean copy: the check first, and the card's button proposed in `format`. */
  | { kind: 'deliver'; format: 'docx' | 'pdf' }
  /** What was delivered last (the receipts). */
  | { kind: 'receipts' }

/** What a turn asks, or null for anything else.
 * - receipts: what was sent / delivered before (a question is the point here:
 *   「我上次发出去的是哪个版本？」「之前交付的是哪一版」).
 * - deliver: a verb that takes revisions / comments / the author out, or asks
 *   for a clean copy (「去掉修订和批注，导出一份 PDF 放桌面」「给我一份干净的版本」);
 *   PDF when the turn says PDF. A question about it (「能去掉批注吗？」) or one
 *   told not to (「不要删批注」) is not a delivery: a question is a check (it
 *   only reads), told not to is null.
 * - check: in a conversation about a document (or naming one in 《》), a check
 *   of what is left before it is sent (「发给对方前帮我检查一下」「看看有没有修订和批注」
 *   「有没有没删干净的东西」). Told not to (「不用检查了」) is null.
 * Never: a turn that is only about something else (「检查一下这段的语法」「批注是什么意思」). */

/** Told not to, immediately before the verb (not 「特别」「区别」). */
const NEGATED = /(?:不要|先别|不用|不必|不想|(?<![特区别])别)\s*(?:删掉|删除|删了|去掉|去除|清除|清理|检查|查一下|查一查|看看|接受|清空|删)/
/** What was already sent or delivered. A question is the point. */
const RECEIPTS = /(?:上次|上一次|之前|以前|先前|最近一次)[^，。？?！!]{0,10}(?:发出去|发出|发给|交付)|交付(?:过|的是|记录|回执)|发出去的是/
const LEFT = '(?:修订|批注|评论|作者|隐藏文字|隐藏)'
/** A verb that takes revisions, comments or the author out, or asks for a clean copy. */
const DELIVER = new RegExp(`(?:去掉|删除|删掉|删了|清除|清理|去除|清空).{0,8}${LEFT}|${LEFT}.{0,6}(?:去掉|删除|删掉|删了|清除|清理|去除|清空)|修订(?:都|全|已)?接受|接受(?:了|掉)?(?:所有|全部|都)?\\s*修订|一份干净|干净的\\s*(?:版|副本|word|pdf|文档|文件)`, 'i')
/** 吗/呢/？ at the end, or 能不能 / 可以吗: a question about cleaning only checks. */
const QUESTION = /[吗呢？?]\s*$|能不能|可以吗/
/** What is still in the document. Bare 「检查语法」 or 「批注是什么意思」 is not this. */
const CHECK = new RegExp(`(?:检查|查一下|查一查|看看|看一下|核对).{0,16}${LEFT}|(?:检查|查一下|查一查|看看|看一下|核对).{0,16}没删干净|有没有(?:没)?删干净|还有没有\\s*${LEFT}|有没有\\s*${LEFT}|没删干净|(?:发给|发出|发送).{0,8}(?:之前|前).{0,16}(?:检查|查一下|查一查|看看)`)

/** `namedBefore` (L9-1c, FU-247): an earlier turn of this conversation named a
 * document in 《》, so 「去掉修订和批注，导出一份 PDF 放桌面」 after 「《合同》发出去前帮我检查一下」
 * is about it (the caller takes that document, `lastNamed`). */
export function asksDeliver(text: string, inDocument: boolean, namedBefore = false): DeliverAsk | null {
  const about = inDocument || namedBefore || /《[^》]{1,200}》/.test(text)
  const said = text.replace(/《[^》]{0,200}》/g, ' ')
  if (NEGATED.test(said)) return null
  if (RECEIPTS.test(said)) return { kind: 'receipts' }
  // A clean copy is of a document: never a turn about anything else.
  if (about && DELIVER.test(said)) {
    if (QUESTION.test(said)) return { kind: 'check' }
    return { kind: 'deliver', format: /pdf/i.test(said) ? 'pdf' : 'docx' }
  }
  if (about && CHECK.test(said)) return { kind: 'check' }
  return null
}

/** The kinds as the user reads them. */
export const KIND_WORDS: Record<DocumentPreflight['findings'][number]['kind'], string> = {
  tracked_insertions: '修订（插入）',
  tracked_deletions: '修订（删除）',
  comments: '批注',
  unfilled_fields: '未填字段',
  author: '作者',
  last_modified_by: '最后修改者',
  hidden_text: '隐藏文字',
  pdf_form: '表单',
  pdf_annotations: '批注',
}

/** What the check found, in words; only the module's facts. Says what was
 * not checked (PR-Daemon #2086 B1) and never calls the document safe. */
const ATTR: ReadonlySet<DocumentPreflight['findings'][number]['kind']> = new Set(['author', 'last_modified_by'])
const DOCX_SCOPE = '只检查了正文、页眉、页脚，脚注、尾注、移动修订、格式修订、样式里设的隐藏没有检查。'
const PDF_SCOPE = '只检查了作者、表单、批注，其他没有检查。'
const DOCX_COPY = '点下面的按钮可以另存一份接受修订、删除批注、清空作者的副本，原文档不动；未填字段和隐藏文字不会自动处理。'
const PDF_ONLY = 'PDF 只能检查，不能清理。'

function findingLine(f: DocumentPreflight['findings'][number]): string {
  const name = KIND_WORDS[f.kind]
  if (ATTR.has(f.kind)) return `• ${name}：${f.examples.join('、')}`
  const examples = f.examples.map((e) => `「${e}」`).join('')
  return `• ${name} ${f.count} 处${examples ? `：${examples}` : ''}`
}

export function preflightText(p: DocumentPreflight): string {
  const title = p.title?.trim() || '文档'
  const head = `《${title}》第 ${p.revision} 版的发送前检查：`
  const body = !p.clean && p.findings.length > 0 ? p.findings.map(findingLine).join('\n') : '检查过的这几类里没有发现问题。'
  const pdf = p.format === 'pdf'
  return `${head}\n${body}\n${pdf ? PDF_SCOPE : DOCX_SCOPE}\n${pdf ? PDF_ONLY : DOCX_COPY}`
}

/** The receipts in words: the latest first, with its source version, format,
 * file name, where and when; `again` said as a second save of the same
 * version; and that a receipt is a file saved on this Mac, not a proof the
 * other side has it. None: says so. */
export function receiptsText(receipts: readonly DeliveryReceipt[], title: string | null): string {
  if (receipts.length === 0) {
    const who = title ? `《${title}》` : ''
    return `${who}还没有交付记录（只记录在本机保存过的副本）。`
  }
  const shown = [...receipts].sort((a, b) => Date.parse(b.at) - Date.parse(a.at)).slice(0, 5)
  const lines = shown.map((r) => {
    const name = r.title?.trim() || title || '文档'
    const when = new Date(r.at).toLocaleString('zh-CN', { hour12: false })
    const again = r.again ? '（同一版本再次保存）' : ''
    const kind = r.format === 'pdf' ? 'PDF' : 'Word'
    return `• 《${name}》第 ${r.revision} 版 → ${kind}「${r.file_name}」，存在 ${r.saved_to}（${when}）${again}`
  })
  return `${lines.join('\n')}\n回执只表示已保存到本机，不表示对方已收到。`
}

/** What the conversation's card holds. */
export interface DeliverCardData {
  documentId: string
  title: string
  revision: number
  cleanable: boolean
  /** The format the turn asked for (the card's first button). */
  suggested: 'docx' | 'pdf'
}

function preflightFailed(code: string): string {
  return code === 'unsupported_format' ? '只能检查 Word 和 PDF 文档。' : `没能完成检查（${code}），文档没有改动。`
}

/** The check of a document, its words and the card. */
export async function runPreflight(api: Api, subject: { id: string; title: string }, ask: DeliverAsk): Promise<{ text: string; card?: DeliverCardData }> {
  try {
    const res = await api.request({ op: 'preflight', documentId: subject.id })
    if (!res.ok) return { text: preflightFailed(res.error.code || 'failed') }
    const p = res.data
    return {
      text: preflightText(p),
      card: { documentId: subject.id, title: subject.title, revision: p.revision, cleanable: p.cleanable, suggested: ask.kind === 'deliver' ? ask.format : 'docx' },
    }
  } catch {
    return { text: preflightFailed('failed') }
  }
}

/** The receipts of one document, or of all when `subject` is null. */
export async function runReceipts(api: Api, subject: { id: string; title: string } | null): Promise<string> {
  try {
    const res = await api.request(subject ? { op: 'receipts', documentId: subject.id } : { op: 'receipts' })
    if (!res.ok) return '没能读取交付记录。'
    return receiptsText(res.data.receipts, subject?.title ?? null)
  } catch {
    return '没能读取交付记录。'
  }
}
