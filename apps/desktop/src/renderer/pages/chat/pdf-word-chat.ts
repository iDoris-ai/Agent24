// L13-1 PDF → Word in a conversation: 「把《X》转成 Word」, or 「把这份转成 Word」
// in a document's conversation, converts like the PDF page's 「转换为 Word」
// (documents/PdfToWord.tsx) without asking the model, the way translation
// does (Chat.tsx D4): the new DOCX goes into the library and is saved to
// Downloads, and the reply says where (裁决 2026-11-28-loop13-chat-to-word).
// A missing converter is downloaded from the document's page; a scan, a
// document that is not a PDF, or a failure is said in one sentence.

import { convert } from '../documents/compose'

type Api = Window['agent24']['documents']
type Sleep = (ms: number) => Promise<unknown>

/** What to say after converting `subject` to Word (and saving it to
 * Downloads). `said` is the user's turn: a 「doc」 there is answered with
 * the .docx the file really is. Never throws for an expected failure. */
export async function runToWord(
  api: Api,
  subject: { id: string; title: string },
  said: string,
  sleep: Sleep = (ms: number) => new Promise((r) => setTimeout(r, ms)),
): Promise<string> {
  const got = await api.request({ op: 'get', documentId: subject.id })
  if (!got.ok) return `没能读取《${subject.title}》，转换没有进行。`
  if (got.data.head.media_type !== 'application/pdf') return `《${subject.title}》不是 PDF，只有 PDF 能转换为 Word。`
  const out = await convert(api, subject.id, undefined, sleep)
  if (!out.ok) {
    if (out.engineMissing && out.engine === 'pdf2docx') {
      return `需要先下载 Word 转换组件：打开《${subject.title}》的文档页，点「转换为 Word」后按提示下载（约 100 MB，只需一次）。`
    }
    if (out.code === 'unsupported_format') return `《${subject.title}》是扫描件（没有文字层），暂时不能转换为 Word。`
    return `没能转换为 Word：${out.message}`
  }
  if (out.documentId === null) return `已把《${subject.title}》转换为 Word，新文档在文档库里。`
  const saved = await api.saveDownload(out.documentId, subject.title)
  if (saved.ok && saved.docxName) {
    const where = `已把《${subject.title}》转换为 Word：下载/${saved.docxName}；新文档也在文档库里。`
    return /\bdoc\b/i.test(said) ? `${where}（文件格式是 .docx。）` : where
  }
  return `已把《${subject.title}》转换为 Word，新文档在文档库里；没能存进「下载」：${saved.message ?? saved.code}`
}
