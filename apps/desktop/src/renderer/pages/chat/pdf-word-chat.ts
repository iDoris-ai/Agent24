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
  void api; void subject; void said; void sleep; void convert
  throw new Error('L13-1: not implemented')
}
