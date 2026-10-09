// OCR for the read engine (ADR-DOC-02 §3.1) comes in the next change. Until
// then, a page PDFKit reads no text from is reported as not read (never as
// empty), and JPEG / PNG files are not read.

import Foundation
import PDFKit

func readByOCR(_ page: PDFPage, ref: CGPDFPage, number: Int, size: CGSize, lines: [Line]) -> ([Line], Unparsed?) {
  guard lines.isEmpty else { return (lines, nil) }
  return (lines, Unparsed(page: number, rects: [[0, 0, Double(size.width), Double(size.height)]], reason: "ocr_failed"))
}

func readImage(_ url: URL) throws -> ([Page], [Unparsed]) {
  throw Failure.unsupported("images are read by OCR, which is not built yet")
}
