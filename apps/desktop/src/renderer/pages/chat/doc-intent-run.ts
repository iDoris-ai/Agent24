// D2b: carries out a document intent (doc-intent.ts) with the documents OS's
// compose and says, in the conversation, what came of it.

import { compose } from '../documents/compose'
import type { DocumentRef } from './doc-context'
import { findByTitle, type DocIntent } from './doc-intent'

type Api = Window['agent24']['documents']

export async function runDocIntent(api: Api, doc: DocumentRef, intent: DocIntent): Promise<string> {
  if (intent.kind === 'split') {
    const title = `${doc.title}（第 ${intent.pages} 页）`.slice(0, 500)
    const out = await compose(api, [{ documentId: doc.id, pages: intent.pages }], title)
    return out.ok
      ? `已把《${doc.title}》的第 ${intent.pages} 页拆成新文档《${title}》，在「文档」列表里可以找到。`
      : `没能拆分：${out.message}`
  }
  const listed = await api.request({ op: 'list', limit: 200 })
  if (!listed.ok) return '没能读取文档列表，合并没有进行。'
  const others = listed.data.documents.filter((d) => d.document_id !== doc.id)
  const other = findByTitle(others, intent.other)
  if (!other) return `没有找到唯一一份标题是《${intent.other}》的文档，请在「文档」页勾选后合并，或说得更完整一些。`
  if (other.media_type !== 'application/pdf') return `《${other.title}》不是 PDF，先把它转换为 PDF 再合并。`
  const title = `合并：${doc.title} + ${other.title}`.slice(0, 500)
  const out = await compose(api, [{ documentId: doc.id }, { documentId: other.document_id }], title)
  return out.ok ? `已合并成新文档《${title}》，在「文档」列表里可以找到。` : `没能合并：${out.message}`
}
