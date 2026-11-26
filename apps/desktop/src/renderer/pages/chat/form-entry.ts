// L9-1b (FU-252, FU-253): which document a form turn fills is told by the
// App, never guessed by the model. The 8B model took the material for the
// template (「把这张入职登记表填一下，资料在《个人信息.docx》里」 → 「没有可填字段」)
// and made up fields for a PDF (「《租赁合同.pdf》这张表能帮我填吗」).
//
// The rule is structural, not a list of words: a named document with
// `{{}}` fields (the read-only `documents.fields`) is a template, one without
// is material; a PDF named as the form is answered from README §15.1 without
// the model; when no template is named, the library's documents whose title
// holds the form's name and that have fields are the candidates, and only
// exactly one is used. Zero or several: the user is asked which, never a guess.

import type { DocumentRef } from './doc-context'
import { findByTitle, formMaterial } from './doc-intent'

type Api = Window['agent24']['documents']

export type FormEntry =
  /** A PDF was named as the form: said plainly not supported (README §15.1). */
  | { kind: 'pdf'; title: string }
  /** The only named document has no `{{}}` and no form is named otherwise (§15.1 row 2). */
  | { kind: 'noFields'; title: string }
  /** Which template is not known for sure: ask, listing the candidates (titles). */
  | { kind: 'choose'; formName: string | null; candidates: string[] }
  /** A named document could not be found or is ambiguous: what namedDocument-style lookup says. */
  | { kind: 'say'; text: string }
  /** The template (and the material, when there is one) go to the model by id. */
  | { kind: 'fill'; template: DocumentRef; material: DocumentRef | null }
  /** The library could not be read: the turn goes to the model as before. */
  | { kind: 'none' }

export interface FormEntryInput {
  /** This conversation's document (F1), or null. */
  doc: DocumentRef | null
  text: string
  /** The earlier user turns, oldest first. */
  earlier: readonly string[]
  /** True when this turn itself asks to fill (asksFillForm), false when it only answers one. */
  asks: boolean
  /** The template this conversation's form fill used last; used only when `asks` is false. */
  remembered: DocumentRef | null
}

/** Words before a form's name that are not part of it. */
const LEAD = /^(?:(?:请|麻烦|帮我|帮忙|给我|替我|你)+(?=再|也|把|将|按|用|给|这|那))?(?:再|也)?(?:把|将|按照|按|用|给)?(?:这|那|该|此|一)?[张份个]?/
/** A form named in words ends in one of these (「报销单」 is a form too). */
const FORM_END = /(表格?|单|模板)$/

/** The form a turn names in words, not in 《》: 「这张入职登记表」→「入职登记表」,
 * 「按报销单模板填」→「报销单」. Null when the turn names none, or only says
 * 「这张表」「表格」「模板」. Text inside 《》 never counts. Only read on a form
 * turn, so 「单」 in 「简单」 is not a worry there. */
export function formNameOf(text: string): string | null {
  const said = text.replace(/《[^》]{0,200}》/g, '，')
  // The run of characters up to the first 表/表格/单/模板 ending, cut at punctuation.
  const m = /([^\s，。、；;,.!?！？《》]{1,24}?)(表格?|单|模板)/.exec(said)
  if (!m) return null
  let name = m[1]!
  // A verb of filling may come right before the name (「帮我填一下请假申请表」).
  const verb = /.*(?:填一下|填写|填好|填)/.exec(name)
  if (verb) name = name.slice(verb[0].length)
  name = name.replace(LEAD, '')
  const end = m[2] === '模板' ? '' : m[2]!
  // 「报销单模板」 names 报销单; a name only of 表 / 模板 names nothing.
  const full = (name + end).replace(/模板$/, '')
  if (full === '' || FORM_END.test(full) && full.replace(FORM_END, '') === '') return null
  return full
}

/** Titles named in 《》, in order, without repeats, at most 3. */
export function namedTitles(text: string): string[] {
  const out: string[] = []
  for (const m of text.matchAll(/《([^》]{1,200})》/g)) {
    const t = m[1]!.trim()
    if (t && !out.includes(t)) out.push(t)
    if (out.length === 3) break
  }
  return out
}

const isPdf = (d: { title: string; media_type?: string }) =>
  d.media_type === 'application/pdf' || /\.pdf$/i.test(d.title.trim())

type Listed = { document_id: string; title: string; media_type: string; head_revision: number; created_at: string }

function refOf(found: { match: Listed; among?: number }): DocumentRef {
  const ref: DocumentRef = { id: found.match.document_id, title: found.match.title, revision: found.match.head_revision }
  if (found.among) {
    const when = found.match.created_at.replace('T', ' ').slice(0, 16)
    ref.note = `文档库里有 ${found.among} 份《${found.match.title}》，用的是最新导入的一份（${when}）。`
  }
  return ref
}

/** Looked up as namedDocument does: the document, or what to say. */
function lookUp(docs: readonly Listed[], named: string): { doc: Listed; ref: DocumentRef } | string {
  const found = findByTitle(docs, named)
  if (!found) return `没有找到标题含《${named}》的文档。`
  if ('candidates' in found) {
    const names = found.candidates.slice(0, 5).map((d) => `《${d.title}》`).join('、')
    return `有几份文档都对得上：${names}。请用完整标题再说一次。`
  }
  return { doc: found.match, ref: refOf(found) }
}

async function hasFields(api: Api, documentId: string): Promise<boolean> {
  try {
    const res = await api.request({ op: 'fields', documentId })
    return res.ok && res.data.fields.length > 0
  } catch {
    // Not readable as a template is not a template.
    return false
  }
}

const strip = (t: string) => t.trim().toLowerCase().replace(/\.(pdf|docx?|xlsx?|pptx?|png|jpe?g)$/, '')

/** The library's non-PDF documents with fields, among those whose title holds `name` (all when null); at most 10 asked. */
async function templatesIn(api: Api, docs: readonly Listed[], name: string | null): Promise<Listed[]> {
  const want = name === null ? null : name.toLowerCase()
  const asked = docs.filter((d) => !isPdf(d) && (want === null || strip(d.title).includes(want))).slice(0, 10)
  const out: Listed[] = []
  for (const d of asked) if (await hasFields(api, d.document_id)) out.push(d)
  return out
}

/** Decide the form turn (see the file comment for the order of the rules):
 * 1. Each title named in 《》 is looked up in one `list` (findByTitle). Not
 *    found / candidates → `say` with the same words as namedDocument
 *    (`没有找到标题含《X》的文档。` / `有几份文档都对得上：…。请用完整标题再说一次。`).
 *    The material told in its own shape (`formMaterial(earlier, text)`) is not
 *    a form candidate.
 * 2. Each named non-PDF is asked `fields`; with at least one field it is a
 *    template. One template → `fill`; several → `choose` with them.
 * 3. No named template, a named PDF that is not the material → `pdf`.
 * 4. In a document's conversation (`doc`) → that document is the template
 *    (`pdf` if it is a PDF).
 * 5. `asks` false and `remembered` → `fill` with it.
 * 6. The form's name (formNameOf on this turn; when `asks` is false, the
 *    newest of the last three earlier turns that names one): the library's
 *    non-PDF documents whose title (extension aside) contains it and that
 *    have fields (asked one by one, at most 10). Exactly one → `fill`; else
 *    `choose`. No name at all: a named plain document that is not the
 *    material → `noFields`; otherwise `choose` with every template found
 *    among the library's non-PDF documents (at most 10 asked), never `fill`.
 * Material: the `formMaterial` title when given (looked up as in 1; not found
 * → `say`); else exactly one named plain document → it; else null.
 * A failed `list` → `none`. A failed `fields` counts as no fields. */
export async function resolveFormEntry(api: Api, input: FormEntryInput): Promise<FormEntry> {
  let docs: Listed[]
  try {
    const listed = await api.request({ op: 'list', limit: 200 })
    if (!listed.ok) return { kind: 'none' }
    docs = listed.data.documents as Listed[]
  } catch {
    return { kind: 'none' }
  }
  const materialTitle = formMaterial(input.earlier, input.text)
  let material: DocumentRef | null = null
  if (materialTitle !== null) {
    const found = lookUp(docs, materialTitle)
    if (typeof found === 'string') return { kind: 'say', text: found }
    material = found.ref
  }
  // 1–2: the documents this turn names, other than the material.
  const templates: DocumentRef[] = []
  const plain: DocumentRef[] = []
  let namedPdf: string | null = null
  for (const title of namedTitles(input.text)) {
    if (title === materialTitle) continue
    const found = lookUp(docs, title)
    if (typeof found === 'string') return { kind: 'say', text: found }
    if (isPdf(found.doc)) namedPdf ??= found.doc.title
    else if (await hasFields(api, found.doc.document_id)) templates.push(found.ref)
    else plain.push(found.ref)
  }
  if (material === null && plain.length === 1) material = plain[0]!
  // An answer that only names the template (「《入职登记表（2026版）.docx》」 after
  // 「要填的是哪一份？」) keeps the material told before (Codex L9-1b C2).
  if (material === null && !input.asks && plain.length === 0 && namedPdf === null && templates.length === 1) {
    const before = formMaterial(input.earlier, '')
    if (before !== null) {
      const found = lookUp(docs, before)
      if (typeof found !== 'string') material = found.ref
    }
  }
  if (templates.length === 1) return { kind: 'fill', template: templates[0]!, material }
  if (templates.length > 1) return { kind: 'choose', formName: null, candidates: templates.map((t) => t.title) }
  // 3–5
  if (namedPdf !== null) return { kind: 'pdf', title: namedPdf }
  if (input.doc) {
    const mine = docs.find((d) => d.document_id === input.doc!.id)
    if (isPdf(mine ?? { title: input.doc.title })) return { kind: 'pdf', title: input.doc.title }
    return { kind: 'fill', template: input.doc, material }
  }
  if (!input.asks && input.remembered) return { kind: 'fill', template: input.remembered, material }
  // 6: the form named in words, in the library.
  const name = formNameOf(input.text)
    ?? (input.asks ? null : input.earlier.slice(-3).reverse().map(formNameOf).find((n) => n !== null) ?? null)
  // Named as the form (not in the material's own shape) and without {{}}: §15.1 row 2.
  if (name === null && plain.length > 0) return { kind: 'noFields', title: plain[0]!.title }
  const found = await templatesIn(api, docs, name)
  if (name !== null && found.length === 1) {
    return { kind: 'fill', template: refOf({ match: found[0]! }), material }
  }
  return { kind: 'choose', formName: name, candidates: found.map((d) => d.title) }
}

/** README §15.1, for a PDF named as a form. */
export function pdfReply(title: string): string {
  return `《${title}》是 PDF，现在填不了：这一版只支持带 {{字段}} 的 Word（DOCX）模板，可填写的 PDF、普通 PDF 和扫描件都不支持。`
    + '可以把它导出成 Word，在要填的地方写上 {{字段名}} 做成模板，再让我填；或者打印出来手填。'
}

/** README §15.1 row 2, for a Word document without `{{}}`. */
export function noFieldsReply(title: string): string {
  return `《${title}》里没有 {{字段}}，现在填不了：这一版只支持带 {{字段}} 的 Word（DOCX）模板。`
    + '可以在要填的地方写上 {{字段名}} 再让我填；或者打印出来手填。'
}

/** 「要填的是哪一份？」 with the candidates, or what to do when there are none. */
export function chooseReply(formName: string | null, candidates: readonly string[]): string {
  if (candidates.length === 0) {
    const what = formName ? `标题含「${formName}」、带 {{字段}} 的模板` : '带 {{字段}} 的模板'
    return `要填的是哪一份？文档库里没有找到${what}。请用《文件名》说出要填的模板；这一版只支持带 {{字段}} 的 Word（DOCX）模板，PDF 和没有 {{}} 的表填不了。`
  }
  const list = candidates.slice(0, 5).map((t) => `《${t}》`).join('、')
  return `要填的是哪一份？找到这几份带 {{字段}} 的模板：${list}。请用《文件名》告诉我。`
}

/** The template by its id, so the model need not tell it from the material. */
export function templateHint(doc: DocumentRef): string {
  return `（这次要填的模板是《${doc.title}》，document_id=${doc.id}：documents.fields 和 documents.fill 的 document_id 都用它，不要把资料文档当模板。）`
}

/** What the App answers by itself, or null when the turn goes to the model. */
export function formEntryReply(entry: FormEntry): string | null {
  switch (entry.kind) {
    case 'pdf': return pdfReply(entry.title)
    case 'noFields': return noFieldsReply(entry.title)
    case 'choose': return chooseReply(entry.formName, entry.candidates)
    case 'say': return entry.text
    default: return null
  }
}
