// L6-S05: 「对比一下这两份」「这一版和上一版改了哪些地方」. Real-Mac lesson of
// F3/F2/F5: a 4-bit model does not pick reliably which two versions or which
// two documents, so the App judges the user's own words, asks the module to
// compare, and writes the summary itself from the module's `changes`; the
// model is not asked and cannot add a change that is not there.

import type { CompareTarget, DocumentChange, DocumentComparison } from '../../../shared/ipc-types'
import { namedDocument } from './doc-intent-run'

type Api = Window['agent24']['documents']

export type CompareAsk =
  | { kind: 'versions'; from: 'previous' | number; to: 'current' | number }
  | { kind: 'documents'; names: string[] }

const VERB = /对比|比较|比对|对照|比一比|比一下/
const CHANGED = /改了(?:哪些|什么)|有什么(?:不同|区别|差别|变化|改动)|哪些(?:不同|区别|差别|变化|改动)|哪里不同|不一样的地方/
const REFUSED = /(?:不要|别|不用|先别|不想|不必|不)\s*(?:再)?\s*(?:对比|比较|比对|对照)/
const NOW = '(?:这一版|这个版本|当前版本|现在的版本|最新版本|最新的版本|这版)'
const BEFORE = '(?:上一版|上一个版本|上个版本|前一版|前一个版本|之前的版本)'
const WITH = '\\s*(?:和|与|跟|同|对比|比较)\\s*'
const PREVIOUS = new RegExp(`${NOW}${WITH}${BEFORE}|${BEFORE}${WITH}${NOW}`)
const NUMBERED = /第\s*(\d{1,3})\s*版\s*(?:和|与|跟|同|对比|比较)\s*第\s*(\d{1,3})\s*版/
const TWO = /这两份|两份(?:文档|文件|合同)?|这两个(?:文档|文件)|两个(?:文档|文件)/

/** Whether a turn asks to compare versions, or documents. */
export function asksCompare(text: string): CompareAsk | null {
  const said = text.replace(/《[^》]{1,200}》/g, '《》')
  if (REFUSED.test(said)) return null
  const asks = VERB.test(said) || CHANGED.test(said)
  if (!asks) return null
  const numbered = NUMBERED.exec(said)
  if (numbered) {
    const [a, b] = [Number(numbered[1]), Number(numbered[2])]
    return { kind: 'versions', from: Math.min(a, b), to: Math.max(a, b) }
  }
  if (PREVIOUS.test(said)) return { kind: 'versions', from: 'previous', to: 'current' }
  const titles = [...text.matchAll(/《([^》]{1,200})》/g)].map((m) => m[1]!.trim()).filter(Boolean)
  if (titles.length >= 2) return { kind: 'documents', names: titles.slice(0, 2) }
  if (TWO.test(said) && VERB.test(said)) return { kind: 'documents', names: [] }
  return null
}

const at = (c: NonNullable<DocumentChange['left']>): string =>
  'block_id' in c.block_ref ? `第 ${c.block_ref.page} 页` : `第 ${c.block_ref.paragraph_index + 1} 段`
const clip = (s: string, n: number): string => (s.length > n ? `${s.slice(0, n)}…` : s)

/** One change as a line: the changed words in 〔〕, the unchanged ones shortened. */
function line(c: DocumentChange): string {
  const side = (c.right ?? c.left)!
  const where = at(side)
  if (c.kind === 'added') return `${c.id} ${where} 新增：「${clip(side.text, 40)}」`
  if (c.kind === 'removed') return `${c.id} ${where} 删除：「${clip(side.text, 40)}」`
  if (c.kind === 'unaligned') return `${c.id} ${where} 无法对齐（可能整段挪了位），不算作改动：「${clip(side.text, 30)}」`
  let out = ''
  const spans = c.spans
  for (let i = 0; i < spans.length; i++) {
    const s = spans[i]!
    const next = spans[i + 1]
    if (s.op === 'equal') out += s.text.length > 24 ? `${s.text.slice(0, 10)}…${s.text.slice(-10)}` : s.text
    else if (s.op === 'delete' && next?.op === 'insert') { out += `〔${s.text}→${next.text}〕`; i++ }
    else out += s.op === 'delete' ? `〔删：${s.text}〕` : `〔加：${s.text}〕`
  }
  return `${c.id} ${where} 修改：${out}`
}

const SHOWN = 8

export interface CompareSummary { head: string; lines: Array<{ id: string; text: string }>; more: number }

/** The facts of a comparison in words; nothing here is the model's. */
export function compareSummary(c: DocumentComparison): CompareSummary {
  const s = c.summary
  const head = c.identical
    ? '没有文字差异 · 只比较文字'
    : `共 ${s.total} 处改动：删 ${s.removed} 处、加 ${s.added} 处、改 ${s.changed} 处、无法对齐 ${s.unaligned} 处 · 只比较文字`
  const lines = c.changes.slice(0, SHOWN).map((x) => ({ id: x.id, text: line(x) }))
  return { head, lines, more: Math.max(0, s.total - lines.length) }
}

export interface CompareSource { documentId: string; revision?: number; label: string }
/** What the conversation's card holds: the two sides and the lines to click. */
export interface CompareCardData { left: CompareSource; right: CompareSource; lines: Array<{ id: string; text: string }> }

const failed = (code: string): string => code === 'unsupported_format'
  ? 'Word 文档和 PDF 不能互相对比，这次没有对比。'
  : `没能对比（${code}），文档没有改动。`

function words(sum: CompareSummary): string {
  const body = sum.lines.map((l) => `• ${l.text}`).join('\n')
  return `${sum.head}${body ? `\n${body}` : ''}${sum.more > 0 ? `\n还有 ${sum.more} 处，请在文档页的对比视图里看。` : ''}`
}

/** The comparison a turn asks for: its summary and the card; or what to
 * say when there is nothing to compare. `named`: titles the conversation
 * named last, to fall back on. */
export async function runCompare(api: Api, ask: CompareAsk, doc: { id: string; title: string } | null, named: string[]): Promise<{ text: string; card?: CompareCardData }> {
  if (ask.kind === 'versions') {
    const subject = doc ?? (named[0] ? await namedDocument(api, named[0]).catch(() => '没能读取文档列表，这次没有对比。') : '请说要对比哪份文档的版本，例如「《合同.docx》这一版和上一版改了哪些地方」。')
    if (typeof subject === 'string') return { text: subject }
    const listed = await api.request({ op: 'revisions', documentId: subject.id }).catch(() => null)
    if (!listed || !listed.ok) return { text: `没能读取《${subject.title}》的版本，这次没有对比。` }
    const head = listed.data.head_revision
    const to = ask.to === 'current' ? head : ask.to
    const from = ask.from === 'previous' ? to - 1 : ask.from
    if (head <= 1) return { text: `《${subject.title}》只有一个版本，没有可以对比的版本。` }
    for (const n of [from, to]) {
      if (n < 1 || n > head) return { text: `《${subject.title}》没有第 ${n} 版，现有第 1～${head} 版。` }
    }
    const [l, r] = [{ documentId: subject.id, revision: from, label: `第 ${from} 版` }, { documentId: subject.id, revision: to, label: `第 ${to} 版` }]
    return finish(api, l, r, `《${subject.title}》第 ${from} 版与第 ${to} 版：`)
  }
  const names = ask.names.length === 2 ? ask.names : named.length >= 2 ? named.slice(0, 2) : []
  if (names.length < 2) return { text: '请说要对比哪两份文档，例如「对比《甲.docx》和《乙.docx》」。' }
  const [a, b] = [await namedDocument(api, names[0]!).catch(() => '没能读取文档列表，这次没有对比。'), await namedDocument(api, names[1]!).catch(() => '没能读取文档列表，这次没有对比。')]
  if (typeof a === 'string') return { text: a }
  if (typeof b === 'string') return { text: b }
  if (a.id === b.id) return { text: '这是同一份文档，要看它的两个版本请说「这一版和上一版改了哪些地方」。' }
  return finish(api, { documentId: a.id, label: a.title }, { documentId: b.id, label: b.title }, `《${a.title}》与《${b.title}》：`)
}

async function finish(api: Api, left: CompareSource, right: CompareSource, lead: string): Promise<{ text: string; card?: CompareCardData }> {
  const req = (s: CompareSource): CompareTarget => (s.revision === undefined ? { documentId: s.documentId } : { documentId: s.documentId, revision: s.revision })
  const res = await api.request({ op: 'compare', left: req(left), right: req(right) }).catch(() => null)
  if (!res) return { text: failed('backend_unreachable') }
  if (!res.ok) return { text: failed(res.error.code) }
  const sum = compareSummary(res.data)
  return { text: `${lead}${words(sum)}`, card: { left, right, lines: sum.lines } }
}
