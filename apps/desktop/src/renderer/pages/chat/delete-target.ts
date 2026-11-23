// L6-F2 (General, 0.6.105 on David's Mac): a 4-bit model asked to delete
// 《X》 called documents.delete with an id it made up, was refused, and then
// asked the user for the id; told to delete 「第 5 份」 it lost the list. The
// App now names the document itself, as F3 does for extraction: this
// conversation's document, or the one 《X》 names. Several answer to the
// name: the user picks one on a card (title, import time, pages; no ids).
// The model is then asked only to call documents.delete with that id, and
// the approval card names the same document.

import { localTime, plainTitle } from './delete-card'

type Api = Window['agent24']['documents']

export interface DeleteCandidate { id: string; title: string; importedAt: string; pages: number | null }

export type DeleteTarget =
  | { kind: 'one'; id: string; title: string }
  | { kind: 'choose'; name: string; total: number; candidates: DeleteCandidate[] }
  /** Nothing to delete: said to the user, the model is not asked. */
  | { kind: 'none'; text: string }

/** Candidates shown on the card; more are counted, not listed. */
export const MAX_CHOICES = 10

/** The document a delete turn means, or null when it names none the App can
 * look up (「删除上一份拆出来的文件」): the model finds that one. */
export async function deleteTargetOf(api: Api, doc: { id: string; title: string } | null, text: string): Promise<DeleteTarget | null> {
  const name = /《([^》]{1,200})》/.exec(text)?.[1]?.trim()
  // 「这份」and the like: this conversation's document. A 《name》 that is not
  // this document's own is looked up: the user means that one.
  if (doc && (!name || plainTitle(doc.title).includes(plainTitle(name)))) return { kind: 'one', id: doc.id, title: doc.title }
  if (!name) return null
  const listed = await api.request({ op: 'list', limit: 200 }).catch(() => null)
  if (!listed || !listed.ok) return { kind: 'none', text: '没能读取文档列表，这次没有删除任何文档。' }
  const docs = listed.data.documents
  const wanted = plainTitle(name)
  // As the documents module resolves a name: the equal titles, else those containing it.
  const exact = docs.filter((d) => plainTitle(d.title) === wanted)
  const hits = exact.length > 0 ? exact : docs.filter((d) => plainTitle(d.title).includes(wanted))
  if (hits.length === 0) return { kind: 'none', text: `文档库里没有找到《${name}》，这次没有删除任何文档。` }
  if (hits.length === 1) return { kind: 'one', id: hits[0]!.document_id, title: hits[0]!.title }
  const candidates = await Promise.all(hits.slice(0, MAX_CHOICES).map(async (d): Promise<DeleteCandidate> => {
    const got = await api.request({ op: 'get', documentId: d.document_id }).catch(() => null)
    const raw = got && got.ok ? (got.data as unknown as { pages?: unknown }).pages : undefined
    const pages = typeof raw === 'number' ? raw : null
    return { id: d.document_id, title: d.title, importedAt: localTime(d.created_at), pages }
  }))
  return { kind: 'choose', name, total: hits.length, candidates }
}

/** The text above the choice card (kept in the history without the buttons). */
export function choiceText(name: string, total: number, shown: number): string {
  const more = total > shown ? `（只列出最近的 ${shown} 份）` : ''
  return `文档库里有 ${total} 份文档都对得上《${name}》，请选要删除的是哪一份${more}：`
}

/** The turn as the model gets it: the one document is fixed, only the call is left. */
export function deletePrompt(target: { id: string; title: string }, text: string): string {
  return `要删除的文档已经确定：《${target.title}》（document_id: ${target.id}）。`
    + `请直接调用一次 documents.delete，document_id 填 ${target.id}；不要再查找别的文档，不要向用户要 id，回答用户时不要提工具名。\n${text}`
}
