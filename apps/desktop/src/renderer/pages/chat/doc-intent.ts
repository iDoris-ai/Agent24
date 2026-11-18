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
const SPLIT_VERB = /(拆|提取|抽取|抽出|取出|单独|另存|导出|摘出)/
const SPLIT_ASKED = /(拆分|拆出|拆成|拆开|拆下|拆一下|单独拆|分割|提取.*页|抽取.*页)/
const MERGE_ASKED = /(合并|合成一份|拼成一份|合在一起)/
const MERGE = /(?:和|与|跟|同)\s*[《「“"]([^》」”"]{1,200})[》」”"]\s*(?:合并|合成|拼|合在一起|放在一起)/

/** What `text` asks for, or null for anything else (an ordinary question). */
export function parseDocIntent(text: string): DocIntent | null {
  const merge = MERGE.exec(text)
  if (merge) return { kind: 'merge', other: merge[1].trim() }
  if (!SPLIT_VERB.test(text)) return MERGE_ASKED.test(text) ? { kind: 'unclear', about: 'merge' } : null
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
/** Prefix the user turn so the Agent uses documents tools (D2b 正式版). */
export function docAgentPrompt(doc: { id: string; title: string } | null, text: string): string {
  const hint = '需要处理文档时请使用 documents 工具（documents.list 查找、documents.compose 拆分或合并生成新文档；document_id 可以直接填文件名）。'
  const head = doc ? `当前对话的文档：《${doc.title}》（document_id: ${doc.id}）。\n` : ''
  return `${head}${hint}\n${text}`
}

export function findByTitle<T extends { document_id: string; title: string }>(
  docs: readonly T[],
  named: string,
): { match: T } | { candidates: T[] } | null {
  const want = named.trim().toLowerCase()
  if (!want) return null
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
