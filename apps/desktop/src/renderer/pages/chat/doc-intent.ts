// D2b（轻量版）: in a conversation about a document (F1, doc-context.ts), a
// plain sentence asks to split pages out of it or to merge it with another
// document, and the page does it with the documents OS's compose, without
// the model: the local model does not call tools reliably (minicpm5-2b writes
// `<function …>` into its text instead of `tool_calls`). The Agent tool comes
// once a model that calls tools is in place.

export type DocIntent =
  | { kind: 'split'; pages: string }
  | { kind: 'merge'; other: string }
  /** Asked to split or merge, but not said which pages / with what: answered
   * with how to say it, never silently left to the model. */
  | { kind: 'unclear'; about: 'split' | 'merge' }

const CN_DIGITS: Record<string, number> = { 零: 0, 〇: 0, 一: 1, 二: 2, 两: 2, 三: 3, 四: 4, 五: 5, 六: 6, 七: 7, 八: 8, 九: 9 }

/** 3, 十二, 二十三, 一百 → a number; anything else null. */
function number(s: string): number | null {
  if (/^\d+$/.test(s)) return Number(s)
  if (!/^[零〇一二两三四五六七八九十百]+$/.test(s)) return null
  let total = 0
  let current = 0
  for (const ch of s) {
    if (ch === '百') { total += (current || 1) * 100; current = 0 } else if (ch === '十') { total += (current || 1) * 10; current = 0 } else current = CN_DIGITS[ch] ?? 0
  }
  const n = total + current
  return n > 0 ? n : null
}

const NUM = '(\\d+|[零〇一二两三四五六七八九十百]+)'
const RANGE = new RegExp(`第?\\s*${NUM}\\s*(?:页)?\\s*(?:[-–—~到至]\\s*第?\\s*${NUM})?\\s*页`, 'g')
/** Verbs that by themselves ask for a new document of pages. */
const SPLIT_VERB = /(拆|分割|另存)/
/** Verbs that also ask questions about pages (「提取第 2 页的关键信息」「第 3 页单独说了
 * 什么」): a split only with a word saying a document is made (PR-Daemon #1909 ①). */
const WEAK_SPLIT_VERB = /(提取|抽取|抽出|取出|单独|导出|摘出)/
/** Only words that make something new: 「文档/文件/PDF」 are in every question
 * about a document (PR-Daemon #1915: 「提取第2页的文件内容」). */
const SPLIT_TARGET = /(存成|存为|另存|新的一份|成一份|单独一份|新文档|新的文档|新文件|新的文件)/
/** A question is never a split, whatever verb it has (PR-Daemon #1915). */
const QUESTION = /(是多少|多少|什么|哪些|哪个|吗|呢|怎么|如何|为什么|[？?])/
const SPLIT_ASKED = /(拆分|拆出|拆成|拆开|拆下|拆一下|单独拆|分割)/
const MERGE_ASKED = /(合并|合成一份|拼成一份|合在一起)/
const MERGE = /(?:和|与|跟|同)\s*[《「“"]([^》」”"]{1,200})[》」”"]\s*(?:合并|合成|拼|合在一起|放在一起)/

/** What `text` asks for, or null for anything else (an ordinary question). */
export function parseDocIntent(text: string): DocIntent | null {
  const merge = MERGE.exec(text)
  if (merge) return { kind: 'merge', other: merge[1].trim() }
  // A document's name is not what is asked: 「《合同.pdf》」 or 「合同.pdf」 says
  // nothing of making a PDF (General 11-19, 0.6.37).
  const said = text
    .replace(/[《「“"][^》」”"]{0,200}[》」”"]/g, ' ')
    .replace(/[^\s，。？！、]+\.(pdf|docx?|xlsx?|pptx?|png|jpe?g)\b/gi, ' ')
  const asksSplit = !QUESTION.test(said) && (SPLIT_VERB.test(said) || (WEAK_SPLIT_VERB.test(said) && SPLIT_TARGET.test(said)))
  if (!asksSplit) return MERGE_ASKED.test(text) ? { kind: 'unclear', about: 'merge' } : null
  const ranges: string[] = []
  for (const m of text.matchAll(RANGE)) {
    const a = number(m[1])
    const b = m[2] === undefined ? null : number(m[2])
    if (a === null || (m[2] !== undefined && b === null)) continue
    if (b !== null && b < a) continue
    ranges.push(b === null || b === a ? String(a) : `${a}-${b}`)
  }
  if (ranges.length) return { kind: 'split', pages: ranges.join(',') }
  if (SPLIT_ASKED.test(text)) return { kind: 'unclear', about: 'split' }
  return MERGE_ASKED.test(text) ? { kind: 'unclear', about: 'merge' } : null
}

/** The document the user named: the only one whose title is exactly it
 * (extension aside); else, among titles containing it, the only shortest one
 * (an original over the documents made from it, 「合并：A + B」 or
 * 「A（第 2 页）」). `candidates` when that still leaves several. */
/** The document a turn outside a document's conversation names first, as
 * 「把《A.pdf》拆出第 1 页」「把《A》和《B》合并」 do; null if none, or if the
 * only one named is the other of a merge. */
export function subjectOf(text: string, intent: DocIntent): string | null {
  const first = /[《「“"]([^》」”"]{1,200})[》」”"]/.exec(text)?.[1]?.trim()
  if (!first) return null
  return intent.kind === 'merge' && first === intent.other ? null : first
}

/** Words that by themselves send a turn to the documents tools (D2b, E1b, D5a). */
const DOC_TOOL_WORDS = /拆|合并|合成|split|merge|combine|模板|填写|填好|填入|生成初稿|修订|改写|template|fill/i
/** M1 (Loop 5): how people also ask to fill or rewrite 「填这份录用通知书」「把第 3 段改得
 * 正式一点」. Everyday words too (「把这句话改成更礼貌的说法」), so they count only
 * in a conversation about a document or with a document named in 《》. */
const DOC_EDIT_WORDS = /填(?:这|进|上|到|一下|《)|改得|改成|改为|润色|正式一点|更正式/
const TRANSLATE_WORDS = /翻译|译成|translate/i
/** M1: 「翻成英文」「译为中文」, everyday words too (「把“你好”翻成英文」): only
 * about a document, as DOC_EDIT_WORDS. */
const TRANSLATE_MORE = /译为|翻成|翻为/

/** In a conversation about a document (`inDocument`), or naming one in 《》. */
function aboutDocument(text: string, inDocument: boolean): boolean {
  return inDocument || /《[^》]{1,200}》/.test(text)
}

/** Whether a turn asks to translate a document (D4). */
export function asksTranslation(text: string, inDocument: boolean): boolean {
  return TRANSLATE_WORDS.test(text) || (aboutDocument(text, inDocument) && TRANSLATE_MORE.test(text))
}

/** Whether a turn is the D4 translation of a document: asked to translate,
 * not to summarise or check, not a split or merge, and about a document (this
 * conversation's, or one named in 《》「」“”). */
export function translatesDocument(text: string, inDocument: boolean): boolean {
  return asksTranslation(text, inDocument)
    && !/总结|摘要|检查|审查|核对/.test(text)
    && parseDocIntent(text) === null
    && (inDocument || /[《「“"][^》」”"]{1,200}[》」”"]/.test(text))
}

/** Whether a turn goes to the Agent with the documents tools. */
export function wantsDocTool(text: string, inDocument: boolean): boolean {
  if (DOC_TOOL_WORDS.test(text)) return true
  return aboutDocument(text, inDocument) && !asksTranslation(text, inDocument) && DOC_EDIT_WORDS.test(text)
}

/** Prefix the user turn so the Agent uses documents tools (D2b 正式版). M1: a
 * split is N calls of compose with one part each; one call with several parts
 * is a merge (a 4-bit model packed a split into one call, 9 runs in 15). */
export function docAgentPrompt(doc: { id: string; title: string } | null, text: string): string {
  const hint = '需要处理文档时请使用 documents 工具（documents.list 查找；documents.compose 每调用一次生成一份新 PDF：合并是一次调用带多个 parts，拆成 N 份是调用 N 次、每次只带一个 part；'
    + '按模板生成：先 documents.fields 查看模板里的 {{字段}}，再用 documents.fill 填写，values 的键是不带花括号的字段名，值取自用户给的信息；'
    + '改写 Word 段落：先 documents.paragraphs 查看段落编号，再用 documents.patch 改写（每段给 index、新文字和 anchor，anchor 必须照抄这一段开头至少 12 个字，少于 12 个字会被拒；编号可以不写，写了要对），tracked=true 写成修订；document_id 可以直接填文件名。回答用户时不要提这些工具名）。'
  const head = doc ? `当前对话的文档：《${doc.title}》（document_id: ${doc.id}）。\n` : ''
  return `${head}${hint}\n${text}`
}

export function findByTitle<T extends { document_id: string; title: string; created_at?: string }>(
  docs: readonly T[],
  named: string,
): { match: T; among?: number } | { candidates: T[] } | null {
  const want = named.trim().toLowerCase()
  if (!want) return null
  // Named with its extension: only that title, extension and all. Several
  // with exactly it (a file imported twice) are one name: the newest is taken,
  // and the caller says which (General 11-18: 「请用完整标题再说一次」 was a dead end).
  if (/\.[a-z0-9]{2,5}$/.test(want)) {
    const same = docs.filter((d) => d.title.trim().toLowerCase() === want)
    if (same.length > 0) {
      const newest = [...same].sort((a, b) => (b.created_at ?? '').localeCompare(a.created_at ?? ''))[0]
      return same.length > 1 ? { match: newest, among: same.length } : { match: newest }
    }
  }
  const strip = (t: string) => t.toLowerCase().replace(/\.(pdf|docx|xlsx|pptx|png|jpe?g)$/, '')
  const exact = docs.filter((d) => strip(d.title) === strip(want))
  if (exact.length === 1) return { match: exact[0] }
  const partial = exact.length > 1 ? exact : docs.filter((d) => d.title.toLowerCase().includes(want))
  if (partial.length === 0) return null
  if (partial.length === 1) return { match: partial[0] }
  const shortest = Math.min(...partial.map((d) => d.title.length))
  const best = partial.filter((d) => d.title.length === shortest)
  return best.length === 1 ? { match: best[0] } : { candidates: partial }
}
