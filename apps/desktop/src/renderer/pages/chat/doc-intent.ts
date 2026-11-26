import { SAVE_HINT, asksSaveToOriginal } from './doc-version'
import type { RunDocuments } from './DocumentsOutcome'
import type { DocumentRef } from './doc-context'

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

/** L6-F2 (PR-Daemon #2028 round 2): a delete of a whole document needs
 * positive evidence, never 「a delete verb and none of some words」: the
 * sentence is the verb with one whole document as its object (「把这份删了」
 * 「删除上一份拆出来的文件」「把《X》删了」), or, in a conversation about a
 * document, the bare verb (「删了吧」). Anything else is not a delete: a miss
 * costs the user a rewording, a false hit proposes a delete. */
const VERB = '(?:删除|删掉|删了|删去|移除)'
/** One whole document. 《…》 is replaced by 《》 first, so a title never counts. */
const NOUN = '(?:文件|文档|PDF|pdf|合同|Word|word)'
const MADE = '(?:拆出来的|生成的|合并的|改写的|填好的|翻译的)'
/** PR-Daemon #2028 round 3: closed words only, never 「any N characters」. */
const ONE_DOC = `(?:《》|[这那该此]?一?份\\s*${NOUN}?|[这那该此]个?\\s*${NOUN}|整份${NOUN}?|上一份${MADE}?(?:那份)?\\s*${NOUN}?|刚才(?:${MADE}|那份)(?:那份|那个)?\\s*${NOUN}?)`
const POLITE_HEAD = '(?:请|麻烦|帮我|你)*\\s*'
/** Only particles, or a reason that it is not needed; never a clause that
 * narrows or takes back the delete (「删了吧，只保留第3页」). */
const TAIL = '(?:吧|掉|了|一下|给我|谢谢|啊|呀)*\\s*(?:[，,](?:它|这份)?(?:都|已经)?(?:过期了|没用了|用不上了|作废了|重复了))?[。！!~～]*'
const OBJECT_FIRST = new RegExp(`^${POLITE_HEAD}(?:把|将)\\s*${ONE_DOC}\\s*(?:给)?${VERB}${TAIL}$`)
const VERB_FIRST = new RegExp(`^${POLITE_HEAD}${VERB}\\s*(?:掉)?\\s*${ONE_DOC}${TAIL}$`)
const BARE = new RegExp(`^(?:那就|好|嗯|行|那)?[，,]?\\s*${POLITE_HEAD}(?:把它|它)?\\s*${VERB}(?:它)?${TAIL}$`)
/** 「不删了」「先别删」「甭删」: told not to, just before the verb. */
const NOT_DELETE = /(?:不|别|甭|不要|不用|先别|先不|无需|不必)\s*(?:删|移除)|算了|不对|开玩笑/
/** Several documents, or a question. */
const SEVERAL = /所有|全部|这些|那些|[这那]?[两三四五六七八九十几]份|[两三四五六七八九十几]个文件|都\s*(?:删|移除)|和|还有|以及|跟/
const DELETE_QUESTION = /怎么|如何|能不能|可以吗|还能|吗|呢|[？?]/

/** Whether a turn asks to delete one whole document. */
export function asksDeleteDocument(text: string, inDocument: boolean): boolean {
  const said = text.trim().replace(/《[^》]{1,200}》/g, '《》')
  if (NOT_DELETE.test(said) || SEVERAL.test(said) || DELETE_QUESTION.test(said)) return false
  return OBJECT_FIRST.test(said) || VERB_FIRST.test(said) || (inDocument && BARE.test(said))
}

/** L7-K0 (FU-184): 「把《X》里的第 3 段删掉」「删除第二段」「第 3 条去掉」: a verb
 * that takes a part away and a numbered part (段/条/句), with only a few
 * words between. It is a rewrite of the document (documents.patch), not a
 * delete of it and not a question for the knowledge bases. Everyday too
 * (「第 3 条建议删掉」 in a chat about anything), so only about a document. */
const PART = '第\\s*[0-9一二三四五六七八九十百零两]+\\s*(?:段|条|句|个段落)'
const TAKE_AWAY = '(?:删除|删掉|删了|删去|删|去掉|去除|移除|拿掉)'
const DELETE_PART = new RegExp(`${TAKE_AWAY}[^，。！？,.!?]{0,4}${PART}|${PART}[^，。！？,.!?]{0,6}${TAKE_AWAY}`)
/** A question about it (「第 3 段怎么删」「要不要删」「会不会影响」), not a request. */
const PART_QUESTION = /怎么|如何|能不能|可以吗|吗|呢|[？?]|为什么|要不要|该不该|会不会|是否|会怎样|的话|哪个/
/** Told not to (「不要删第 3 段」「不要把第 3 段删掉」「别删」「第 3 段不用删」), on top of
 * NOT_DELETE (a refusal just before the verb): a miss costs a rewording, a
 * false hit proposes the opposite of what was said. 「别」 counts only before a
 * verb (not 「区别」「特别」). */
const PART_NEGATED = /不要|不用|不必|无需|不需要|没必要|不想|先不|不许|不准|禁止|甭|不删|不能|别(?=\s*(?:把|删|去|移|拿|动))/

/** Whether a turn asks to take one numbered part out of a document. */
export function asksDeleteParagraph(text: string, inDocument: boolean): boolean {
  if (!aboutDocument(text, inDocument)) return false
  // A title is never the sentence (《不对的合同.docx》); as asksDeleteDocument does.
  const said = text.replace(/《[^》]{1,200}》/g, '《》')
  if (/[？?]\s*$/.test(said)) return false
  // Told not to, or asking, only within the clause that holds the delete
  // (「删掉第3段，其他不要动」 is a request with a limit, not a refusal).
  return said.split(/[，。！？,.!?]/).some((clause) =>
    DELETE_PART.test(clause) && !NOT_DELETE.test(clause) && !PART_NEGATED.test(clause) && !PART_QUESTION.test(clause))
}

/** L8-S02: 「把这张入职登记表填一下」「这张表能填吗？」「按《模板》填写」: a verb of
 * filling, about a form (表, a template) or a document. */
const FILL_VERB = /填写|填好|填入|填上|填一下|填了|填完|填表|能填|可以填|能不能填/
const FORM_NOUN = /表|模板|《[^》]{1,200}》/

/** Whether a turn asks to fill a form, or whether one can be. */
export function asksFillForm(text: string, inDocument: boolean): boolean {
  return FILL_VERB.test(text) && (inDocument || FORM_NOUN.test(text))
}

/** L8-S02: the answers to what a fill asked (「电话是 139…」「紧急联系人先空着」)
 * belong to it: a form turn among the last three before this one (`before`:
 * the earlier user turns, oldest first). */
export function continuesFormFill(before: readonly string[], text: string, inDocument = false): boolean {
  return text.trim() !== '' && before.slice(-3).some((t) => asksFillForm(t, inDocument))
}

/** Whether a turn goes to the Agent with the documents tools. */
export function wantsDocTool(text: string, inDocument: boolean): boolean {
  if (DOC_TOOL_WORDS.test(text) || asksFillForm(text, inDocument) || asksDeleteDocument(text, inDocument) || asksDeleteParagraph(text, inDocument)) return true
  return aboutDocument(text, inDocument) && !asksTranslation(text, inDocument) && DOC_EDIT_WORDS.test(text)
}

/** Prefix the user turn so the Agent uses documents tools (D2b 正式版). M1: a
 * split is N calls of compose with one part each; one call with several parts
 * is a merge (a 4-bit model packed a split into one call, 9 runs in 15). */
/** L7-K0: only on a turn that takes a numbered part out of a document. */
const PARAGRAPH_HINT = '（删掉文档里的某一段是改写，用 documents.patch，不是删除整份文档；先用 documents.paragraphs 核对那一段，不要用 documents.delete；删除就是把该段 text 置空（留一个空段），不要编替代文字。）'
/** L6-F2: only on a turn that asks to delete a whole document. */
const DELETE_HINT = '（删除整份文档用 documents.delete，只删用户说的那一份；document_id 必须是 doc_ 开头的 id，不要自己编，也不要向用户要：当前对话的文档直接用它的 document_id，'
  + '「上一份」「刚才拆出来的」先用 documents.list 按导入时间找，按文件名先用 documents.get 查；同名的有多份时把候选列给用户选，不要自己挑；删一次就结束。删掉文档里的某一段、某一句是改写，不是删除文档。）'

/** L8-S02 (David 11-26, 方案 A): only on a form turn, or an answer to one. */
export const FORM_HINT = '（填表：只支持带 {{字段}} 的 Word（DOCX）模板。用户给了资料文档时，每次调用 documents.fill 之前先调用 documents.fields，带 source_document_id=资料文档；不用自己读资料：字段里的 from_source 就是资料里对上的值（只有部门、岗位、入职日期、学历这类岗位字段），原样放进 documents.fill 的 values。'
  + '其余字段（包括 kind 为 identity 的姓名、证件号、电话、地址、银行卡、联系人、甲乙方等）一律不从资料里取，即使资料里看得到也要逐项问用户，不要自己编；用户亲口给的值放进 values，字段名放进 user_said；用户说先空着的，values 里给空字符串；kind 为 manual 的（签名、照片、勾选框、附件）不要填，告诉用户需要手工处理。'
  + '带资料时，资料里的岗位字段要等用户亲口说了姓名（放进 user_said）、并且和资料里的姓名一致才会写入；问齐了或用户说先空着、确认生成时，必须调用 documents.fill，带同一个 source_document_id；没有调用成功就不要说已生成或已保存。填完把答复里的 note 转告用户，ask_user 里的逐项问，written 为 false 表示没有生成文档。'
  + 'PDF 表单（可填写的 PDF、普通 PDF、扫描件）和没有 {{}} 的表都填不了：如实说现在不支持，建议导出成 Word 后改成带 {{字段}} 的模板，或打印后手填，不要调用 documents.fill。）'

/** PR-Daemon #2100 r4 E1/E3 (监工): a clause (split at 「，。；,;!?！？」 and line
 * breaks) names the material only when the whole of it is one of these, with
 * nothing before or after: no list of words of no to keep up. */
const CLAUSE = /[，。；,;!?！？\n]/
const SHAPES = [
  /^(?:请|我的|这个)?资料(?:在|是)《([^》]{1,200})》(?:里|中)?$/,
  /^用《([^》]{1,200})》(?:里|中)的资料(?:来)?(?:填)?$/,
]
/** r4 E2: a turn with any of these names no earlier material either. */
const TAKE_BACK = /资料|那份|这份|上份|《|》|不|别|没|勿|无需|莫|非|忽略|改用|换|撤|算了/
/** Another clause about a material (「这份别用」「改用…的资料」). */
const OTHER_MATERIAL = /资料|那份|这份|上份/

/** The material a turn names, or null: exactly one clause in one of the
 * shapes, and no other clause about a material. */
function namedMaterial(t: string): string | null {
  const clauses = t.split(CLAUSE).map((c) => c.trim()).filter((c) => c !== '')
  let title: string | null = null
  for (const c of clauses) {
    const m = SHAPES.map((s) => s.exec(c)).find((x) => x !== null)
    if (m) {
      if (title !== null) return null
      title = m[1]!.trim()
    } else if (OTHER_MATERIAL.test(c)) return null
  }
  return title
}

/** 监工 0.7.123 真机: the material a form fill was told, as a title; null when
 * none. This turn first; an earlier one (the last three, newest first) only
 * when this turn says nothing of a material or of no (宁可不认). */
export function formMaterial(before: readonly string[], text: string): string | null {
  const here = namedMaterial(text)
  if (here !== null) return here
  if (TAKE_BACK.test(text)) return null
  for (const t of before.slice(-3).reverse()) {
    const m = namedMaterial(t)
    if (m !== null) return m
  }
  return null
}

/** The material by its id, so the model need not find it (it called fields
 * without it and left the material's values out of fill). */
export function materialHint(doc: DocumentRef): string {
  return `（这次的资料文档是《${doc.title}》，source_document_id=${doc.id}：调用 documents.fields 和 documents.fill 时都带上它。）`
}

/** 「已生成」「已保存为…」「填好了」: an answer that says a document was made or saved. */
const CLAIMS_MADE = /已(?:经)?(?:为(?:你|您))?(?:生成|保存|填好|填写完|创建|做好)|(?:生成|填|做)好了|生成了|保存(?:为|到|在|了)|文档已/

/** L8-S02 真机 0.7.122 (监工): a form turn's answer said 「已生成…，已保存为…」
 * while the run called only documents.fields. Said by the page from the run's
 * record, never the model's words: no write in a form turn whose answer
 * claims one reads as 「这次没有改动文档」. Any other turn, or a run that did
 * write, keeps what it had. */
export function formClaimUnbacked(prompt: string, answer: string, documents: RunDocuments | null | undefined): RunDocuments | null | undefined {
  if (documents || !prompt.includes(FORM_HINT) || !CLAIMS_MADE.test(answer)) return documents ?? null
  return { attempted: true, jobs: [], failed: [] }
}

/** `form`: the turn fills a form, or answers what one asked (Chat.tsx, `continuesFormFill`). */
export function docAgentPrompt(doc: { id: string; title: string } | null, text: string, form = asksFillForm(text, doc !== null), material = ''): string {
  const hint = '需要处理文档时请使用 documents 工具（documents.list 查找；documents.compose 每调用一次生成一份新 PDF：合并是一次调用带多个 parts，拆成 N 份是调用 N 次、每次只带一个 part；'
    + '按模板生成：先 documents.fields 查看模板里的 {{字段}}，再用 documents.fill 填写，values 的键是不带花括号的字段名，值取自用户给的信息，这些字段名都放进 user_said；'
    + '改写 Word 段落：先 documents.paragraphs 查看段落编号，再用 documents.patch 改写（每段给 index、新文字和 anchor，anchor 必须照抄这一段开头至少 12 个字，少于 12 个字会被拒；编号可以不写，写了要对），tracked=true 写成修订；document_id 可以直接填文件名；compose、fill、patch 成功一次就结束，不要重复调用。回答用户时不要提这些工具名）。'
  const head = doc ? `当前对话的文档：《${doc.title}》（document_id: ${doc.id}）。\n` : ''
  // L6-F5: only a turn the App judged to save into the original says so.
  const save = asksSaveToOriginal(text) ? SAVE_HINT : ''
  return `${head}${hint}${asksDeleteDocument(text, doc !== null) ? DELETE_HINT : ''}${asksDeleteParagraph(text, doc !== null) ? PARAGRAPH_HINT : ''}${save}${form ? FORM_HINT + material : ''}\n${text}`
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

/** F3 (Loop 6): 「把这份合同的甲方、乙方、金额、期限抽出来」「…是多少，列出来」
 * 「从这份通知里找出截止日期和要交的材料」. 找出 alone is everyday too
 * (「找出原因」), so it counts only with what a document states. Longer verbs
 * first: 「抽取出来」 is not 「抽取」 and a field 「出来」. */
const EXTRACT_VERB = /抽取出来|提取出来|抽出来|摘出来|列出来|抽取/
const FIND_VERB = /找出来?/
const FIND_WHAT = /日期|时间|期限|金额|价格|费用|总价|总额|名称|编号|号码|地址|电话|材料|联系人|甲方|乙方/
/** A question about the document (「这个字段怎么填」) or another task. */
const NOT_EXTRACTION = /怎么|如何|为什么|什么意思|填|改写|改成|改得|翻译|译成|总结|摘要|检查|审查/
/** 「这份合同的」「这张发票的」「这份通知里」 before the fields. */
const DOC_PREFIX = /^(?:请|帮我)?\s*(?:把|将|从|在)?\s*(?:这|该|本|此|那)[份张个篇封]?[\u4e00-\u9fa5A-Za-z0-9]{0,8}?(?:里面?的?|中的?|上的?|的)/
const FIELD_TAIL = /(?:分别|都)?(?:是)?(?:多少|什么|哪些|哪天|几号|哪一天|谁)?[？?]?$/
/** A question, not a request (「你能列出来这份文档的大纲吗」, PR-Daemon #2019 ①). */
const ASKS = /(?:吗|呢)[？?]?$|[？?]$/
/** Words after the verb that ask nothing more (「…抽出来，谢谢」). */
const POLITE = /^(?:来|一下|吧|给我|谢谢|多谢|麻烦了|啊|呀|哦|就行|即可|[，,。！!~～\s])*$/
/** What a field is called: its name ends in a word for something a document
 * states (PR-Daemon #2019 ①, round 3). Anything else (「合同的漏洞」「优缺点」
 * 「截止日期并设个提醒」「乙方的区别」) is not a list of fields: the model answers. */
const FIELD_NOUN = /(?:日期|时间|期限|期|日|金额|总额|税额|余额|金|价格|单价|总价|价|费用|费|税率|名称|姓名|名字|编号|号码|单号|账号|税号|证号|地址|地点|电话|手机|邮箱|联系人|负责人|代表人|收款人|开票人|甲方|乙方|丙方|买方|卖方|单位|公司|银行|账户|数量|面积|材料|清单|附件|方式|条件|期数|(?:甲方|乙方|联系|付款|开票|收款|银行|账户|公司)信息)$/
/** A comparison is not a field (「与上一版不同的金额」). */
const COMPARES = /不同|区别|差异|变化|比较|对比|上一版|有问题|不合理|不一致|矛盾|错|不利|风险/
/** Something done after, or a person (「找出截止日期并发到我的邮箱」「…告诉负责人」
 * 「你推荐的付款方式」): a name with these is a request, not a field (#2019 round 4). */
const DOES = /并|之后|然后|以后|以便|发(?!票)|告诉|提醒|做成|核对|(?<![结计预核])算|你|我/

/** The field names a turn asks to extract from its document: null when it
 * asks for something else; [] when it asks to extract but names no field
 * the rules can read (the model is then asked only for the names). Judged by
 * the sentence's shape (PR-Daemon #2019 ①): 「把 X 抽出来」 takes X, and
 * anything said after the verb (「…，到时候提醒我」「…算个总数」) is more than an
 * extraction; 「找出/列出来 X」 takes X, up to the end of its clause. */
export function extractionFields(text: string, inDocument: boolean): string[] | null {
  if (!aboutDocument(text, inDocument) || NOT_EXTRACTION.test(text) || ASKS.test(text.trim()) || parseDocIntent(text) !== null) return null
  const said = text.replace(/《[^》]{1,200}》/g, '这份')
  const verb = EXTRACT_VERB.exec(said) ?? (FIND_WHAT.test(said) ? FIND_VERB.exec(said) : null)
  if (!verb) return null
  const SEP = /[，。,；;！!\n]/
  const head = said.slice(0, verb.index)
  const rest = said.slice(verb.index + verb[0].length)
  // The verb's own clause before it, without where the fields are (「从这份通知里」).
  const own = (head.split(SEP).pop() ?? '').replace(DOC_PREFIX, '').replace(/^(?:请|帮我|给我)?\s*(?:从|在)?\s*(?:这|该|本|此)[份张个篇封]?[\u4e00-\u9fa5]{0,8}?(?:里面?|中|上)$/, '').trim()
  const restClauses = rest.split(SEP).map((c) => c.trim().replace(/^(?:来|一下|吧|给我)+/, ''))
  let list: string
  if (own.length > 0 || restClauses[0].length === 0) {
    // 「把 X 抽出来」「X 是多少，列出来」: nothing but politeness may follow.
    if (!POLITE.test(rest)) return null
    list = own.length > 0 ? own : (head.split(SEP).map((c) => c.trim()).filter(Boolean).pop() ?? '')
  } else {
    // 「找出 X」「列出来 X」: X is the clause after the verb; nothing more may follow.
    if (/然后|并且|再/.test(restClauses[0]) || !POLITE.test(restClauses.slice(1).join(''))) return null
    list = restClauses[0]
  }
  list = list.replace(DOC_PREFIX, '').replace(/(?:帮我|给我|请)?(?:都)?$/, '').replace(FIELD_TAIL, '').replace(/等(?:信息|内容|字段)?$|这些(?:信息|内容|字段)?$/, '')
  const names = list.split(/以及|还有|[、，,和与及跟/]/).map((n) => n.replace(/^(?:请|帮我|麻烦)?(?:把|将|的)*/, '').trim()).filter((n) => n.length > 0 && n.length <= 30)
  if (names.some((n) => !FIELD_NOUN.test(n) || COMPARES.test(n) || DOES.test(n))) return null
  return [...new Set(names)].slice(0, 20)
}
