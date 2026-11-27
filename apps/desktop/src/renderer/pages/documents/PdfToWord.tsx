// L12-3 PDF → Word: 「转换为 Word」 on a PDF document's page. The documents OS
// converts with the a24-pdf2docx engine (rust/apps/agent24-documents/src/pdf_word.rs),
// downloaded on demand like LibreOffice (main/pdf-word-engine.ts); the new DOCX
// document is imported next to the PDF. A scan (no text layer) is refused.

type Api = Window['agent24']['documents']

export default function PdfToWord(props: { api: Api; documentId: string; onAdded?: () => void }) {
  void props
  throw new Error('L12-3: todo')
}
