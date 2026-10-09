// OCR for the read engine (ADR-DOC-02 §3.1): pages, or parts of pages, that
// PDFKit cannot read as text, and JPEG / PNG files, are read by Vision.

import AppKit
import Foundation
import ImageIO
import PDFKit
import Vision

/// Pixels in one OCR render: the page at 2x, or smaller if that is larger.
let maxPixels = 40_000_000.0

/// What a content-stream scan has found so far. A scan cut short by its
/// limits, or that fails, counts as finding an image: the page is read by
/// OCR rather than risk missing a scan.
final class Scan {
  var found = false
  var depth = 0
  var forms = 0
  var table: CGPDFOperatorTableRef?
}

/// Whether drawing `stream` paints an image: an image XObject, an inline
/// image, or either inside a form XObject (to 8 levels and 256 forms, so a
/// form that draws itself ends; past them, assume one).
func drawsImage(_ stream: CGPDFContentStreamRef) -> Bool {
  let scan = Scan()
  guard let table = CGPDFOperatorTableCreate() else { return true }  // unknown: read it by OCR
  scan.table = table
  let image: CGPDFOperatorCallback = { _, info in
    Unmanaged<Scan>.fromOpaque(info!).takeUnretainedValue().found = true
  }
  CGPDFOperatorTableSetCallback(table, "BI", image)
  CGPDFOperatorTableSetCallback(table, "EI", image)
  CGPDFOperatorTableSetCallback(table, "Do") { scanner, info in
    let scan = Unmanaged<Scan>.fromOpaque(info!).takeUnretainedValue()
    var name: UnsafePointer<CChar>?
    let outer = CGPDFScannerGetContentStream(scanner)
    guard !scan.found, CGPDFScannerPopName(scanner, &name), let n = name,
      let obj = CGPDFContentStreamGetResource(outer, "XObject", n)
    else { return }
    var xobject: CGPDFStreamRef?
    var subtype: UnsafePointer<CChar>?
    guard CGPDFObjectGetValue(obj, .stream, &xobject), let x = xobject,
      let dict = CGPDFStreamGetDictionary(x), CGPDFDictionaryGetName(dict, "Subtype", &subtype),
      let s = subtype
    else { return }
    switch String(cString: s) {
    case "Image": scan.found = true
    case "Form" where scan.depth >= 8 || scan.forms >= 256: scan.found = true
    case "Form":
      scan.forms += 1
      // A form without resources of its own looks names up in its parent's
      // (`outer`); its stream dictionary stands in, holding none.
      var res: CGPDFDictionaryRef?
      _ = CGPDFDictionaryGetDictionary(dict, "Resources", &res)
      let inner = CGPDFContentStreamCreateWithStream(x, res ?? dict, outer)
      let sub = CGPDFScannerCreate(inner, scan.table, info)
      scan.depth += 1
      if !CGPDFScannerScan(sub) { scan.found = true }
      scan.depth -= 1
      CGPDFScannerRelease(sub)
      CGPDFContentStreamRelease(inner)
    default: break
    }
  }
  let scanner = CGPDFScannerCreate(stream, table, Unmanaged.passUnretained(scan).toOpaque())
  if !CGPDFScannerScan(scanner) { scan.found = true }
  CGPDFScannerRelease(scanner)
  CGPDFOperatorTableRelease(table)
  return scan.found
}

/// Text lines Vision reads in `image` (shown as `orientation`), in points
/// of a `size` page, top left origin.
func ocr(_ image: CGImage, orientation: CGImagePropertyOrientation = .up, size: CGSize) throws -> [Line] {
  let request = VNRecognizeTextRequest()
  request.recognitionLevel = .accurate
  request.recognitionLanguages = ["zh-Hans", "zh-Hant", "en-US"]
  request.usesLanguageCorrection = true
  try VNImageRequestHandler(cgImage: image, orientation: orientation, options: [:]).perform([request])
  let (w, h) = (Double(size.width), Double(size.height))
  return (request.results ?? []).compactMap { obs -> Line? in
    guard let raw = obs.topCandidates(1).first?.string else { return nil }
    let text = unligated(raw).split(whereSeparator: \.isNewline).joined(separator: " ")
    if text.isEmpty { return nil }
    let b = obs.boundingBox  // normalized, origin bottom left, as displayed
    return Line(text: text, rect: [b.minX * w, (1 - b.maxY) * h, b.maxX * w, (1 - b.minY) * h])
  }
}

/// Rows top to bottom (a line joins the row whose first line its top is
/// within half of), each row left to right.
func readingOrder(_ lines: [Line]) -> [Line] {
  var rows: [[Line]] = []
  for line in lines.sorted(by: { ($0.rect[1], $0.rect[0]) < ($1.rect[1], $1.rect[0]) }) {
    if let first = rows.last?.first, line.rect[1] < first.rect[1] + (first.rect[3] - first.rect[1]) / 2 {
      rows[rows.count - 1].append(line)
    } else {
      rows.append([line])
    }
  }
  return rows.flatMap { $0.sorted { $0.rect[0] < $1.rect[0] } }
}

/// Whether `native` already says everything `read` does, spaces aside. An
/// OCR reading that says more is kept, even if that repeats some text.
func says(_ native: String, all read: String) -> Bool {
  let (x, y) = (native.filter { !$0.isWhitespace }, read.filter { !$0.isWhitespace })
  return !y.isEmpty && x.contains(y)
}

/// The page as displayed, at 2x within the pixel budget, for OCR.
func render(_ page: PDFPage, size: CGSize) -> CGImage? {
  let scale = min(2, (maxPixels / Double(size.width * size.height)).squareRoot())
  let image = page.thumbnail(of: NSSize(width: size.width * scale, height: size.height * scale), for: .cropBox)
  var rect = NSRect(origin: .zero, size: image.size)
  return image.cgImage(forProposedRect: &rect, context: nil, hints: nil)
}

/// A PDF page's lines with what OCR adds: on a page with no text, or that
/// draws an image anywhere, the rendered page is recognised. OCR of text
/// PDFKit already read (overlapping it, and saying nothing more) is left out.
/// A page that cannot be rendered or recognised is returned as unparsed.
func readByOCR(_ page: PDFPage, ref: CGPDFPage, number: Int, size: CGSize, lines: [Line]) -> ([Line], Unparsed?) {
  let content = CGPDFContentStreamCreateWithPage(ref)
  defer { CGPDFContentStreamRelease(content) }
  guard lines.isEmpty || drawsImage(content) else { return (lines, nil) }
  let whole = [0, 0, Double(size.width), Double(size.height)]
  guard let image = render(page, size: size) else {
    return (lines, Unparsed(page: number, rects: [whole], reason: "render_failed"))
  }
  do {
    let read = try ocr(image, size: size).filter { o in
      !lines.contains { covered(o.rect, by: $0.rect) > 0.5 && says($0.text, all: o.text) }
    }
    return (read.isEmpty ? lines : readingOrder(lines + read), nil)
  } catch {
    return (lines, Unparsed(page: number, rects: [whole], reason: "ocr_failed"))
  }
}

/// A JPEG or PNG: one page as displayed (EXIF orientation applied), its
/// pixels taken as points.
func readImage(_ url: URL) throws -> ([Page], [Unparsed]) {
  guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
    let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
  else { throw Failure.failed("the image cannot be decoded") }
  let props = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
  let raw = (props?[kCGImagePropertyOrientation] as? UInt32) ?? 1
  let orientation = CGImagePropertyOrientation(rawValue: raw) ?? .up
  let size = (5...8).contains(raw)
    ? CGSize(width: image.height, height: image.width) : CGSize(width: image.width, height: image.height)
  do {
    let lines = readingOrder(try ocr(image, orientation: orientation, size: size))
    return ([Page(page: 1, width: Double(size.width), height: Double(size.height), lines: lines)], [])
  } catch {
    throw Failure.failed("OCR failed: \(error.localizedDescription)")
  }
}
