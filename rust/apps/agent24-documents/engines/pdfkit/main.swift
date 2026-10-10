// agent24-documents-pdfkit: the slice-1 read engine of the Documenting OS
// (ADR-DOC-01 D6 amendment, ADR-DOC-02 §3.1, §4). One run reads one file.
// No network, no files written.
//
//   agent24-documents-pdfkit parse <path> <media-type>
//   agent24-documents-pdfkit render <path> <media-type> <page> <scale> <max-bytes> [x0,y0,x1,y1]
//
// `parse` (protocol 1, only on exit 0) writes one JSON object: each page's
// size and its lines in reading order, each a text and a rectangle in CropBox
// points, origin top left of the page as displayed (after /Rotate); and the
// regions not read. The OS turns that into blocks and pins the text layer.
// `render` (only on exit 0) writes one JSON line, the scale used and the
// image's size, then a PNG of at most <max-bytes> (render.swift).
// Exit 2: not a format this engine reads. Exit 3: a file of that format that
// could not be read (stderr says why). Render only: exit 4, no such page;
// 5, the region is empty or off the page; 6, too large even at the smallest
// scale. Exit 64: usage.

import AppKit
import Foundation
import PDFKit

struct Line: Encodable { let text: String; let rect: [Double] }
struct Page: Encodable { let page: Int; let width: Double; let height: Double; let lines: [Line] }
struct Unparsed: Encodable { let page: Int; let rects: [[Double]]; let reason: String }
struct Output: Encodable { let `protocol`: Int; let os_version: String; let pages: [Page]; let unparsed: [Unparsed] }
enum Failure: Error { case unsupported(String), failed(String), noPage, offPage, tooLarge }

/// Typographic ligatures (U+FB00–U+FB06) are how a PDF draws letters, not
/// text: they are read as the letters, so "shut-oﬀ" is found as "shut-off".
/// Nothing else is normalized (§3.1).
func unligated(_ s: String) -> String {
  let map: [Character: String] = [
    "\u{FB00}": "ff", "\u{FB01}": "fi", "\u{FB02}": "fl", "\u{FB03}": "ffi", "\u{FB04}": "ffl",
    "\u{FB05}": "st", "\u{FB06}": "st",
  ]
  return s.contains(where: { map[$0] != nil }) ? s.map { map[$0] ?? String($0) }.joined() : s
}

/// A page-space rectangle (origin bottom left, unrotated) in the page's
/// CropBox coordinates, origin top left of the page as displayed.
func displayed(_ r: CGRect, crop: CGRect, rotation: Int) -> [Double] {
  let (w, h) = (Double(crop.width), Double(crop.height))
  let x0 = Double(r.minX - crop.minX), x1 = Double(r.maxX - crop.minX)
  let y0 = h - Double(r.maxY - crop.minY), y1 = h - Double(r.minY - crop.minY)
  func turn(_ x: Double, _ y: Double) -> (Double, Double) {
    switch rotation {
    case 90: return (h - y, x)
    case 180: return (w - x, h - y)
    case 270: return (y, w - x)
    default: return (x, y)
    }
  }
  let (ax, ay) = turn(x0, y0), (bx, by) = turn(x1, y1)
  return [min(ax, bx), min(ay, by), max(ax, bx), max(ay, by)].map { max(0, $0) }
}

/// Share of `a`'s area that `b` covers.
func covered(_ a: [Double], by b: [Double]) -> Double {
  let w = min(a[2], b[2]) - max(a[0], b[0]), h = min(a[3], b[3]) - max(a[1], b[1])
  let area = (a[2] - a[0]) * (a[3] - a[1])
  return w > 0 && h > 0 && area > 0 ? w * h / area : 0
}

/// One selection line's parts: it may span a break, and a line never holds
/// one (§3.1). The parts share the selection's bounds: PDFKit's page text
/// and its selections do not always agree on offsets, so finer bounds are
/// not to be had reliably, and a wider rectangle never misses the text.
func parts(of sel: PDFSelection, on page: PDFPage) -> [(String, CGRect)] {
  let whole = sel.bounds(for: page)
  return (sel.string ?? "").split(whereSeparator: \.isNewline).map { (String($0), whole) }
}

func readPDF(_ url: URL) throws -> ([Page], [Unparsed]) {
  guard let doc = PDFDocument(url: url) else { throw Failure.failed("PDFKit cannot open this PDF") }
  if doc.isLocked { throw Failure.failed("the PDF is encrypted") }
  var pages: [Page] = []
  var unparsed: [Unparsed] = []
  for i in 0..<doc.pageCount {
    try autoreleasepool {
      guard let page = doc.page(at: i), let ref = page.pageRef else {
        throw Failure.failed("page \(i + 1) cannot be opened")
      }
      let crop = page.bounds(for: .cropBox)
      let rotation = ((page.rotation % 360) + 360) % 360
      let size = rotation == 90 || rotation == 270 ? CGSize(width: crop.height, height: crop.width) : crop.size
      var lines: [Line] = []
      for sel in page.selection(for: crop)?.selectionsByLine() ?? [] {
        let mine = parts(of: sel, on: page).map {
          Line(text: unligated($0.0), rect: displayed($0.1, crop: crop, rotation: rotation))
        }
        // Text drawn twice in place (a fake bold) comes back as a second
        // selection: one that lies (80%) within an earlier line and says
        // nothing that line does not, is that line again.
        lines += mine.filter { l in !lines.contains { $0.text.contains(l.text) && covered(l.rect, by: $0.rect) > 0.8 } }
      }
      // What PDFKit cannot read as text (scans, images) is read by OCR.
      let (all, missed) = readByOCR(page, ref: ref, number: i + 1, size: size, lines: lines)
      lines = all
      if let missed { unparsed.append(missed) }
      pages.append(Page(page: i + 1, width: Double(size.width), height: Double(size.height), lines: lines))
    }
  }
  return (pages, unparsed)
}

/// The format the file's first bytes show, whatever its name says.
func sniff(_ url: URL) throws -> String {
  guard let handle = try? FileHandle(forReadingFrom: url) else { throw Failure.failed("the file cannot be read") }
  defer { try? handle.close() }
  guard let data = try? handle.read(upToCount: 8) else { throw Failure.failed("the file cannot be read") }
  let head = [UInt8](data)
  if head.starts(with: Array("%PDF-".utf8)) { return "application/pdf" }
  if head.starts(with: [0xFF, 0xD8, 0xFF]) { return "image/jpeg" }
  if head.starts(with: [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) { return "image/png" }
  return "?"
}

func fail(_ code: Int32, _ why: String) -> Never {
  FileHandle.standardError.write("\(why)\n".data(using: .utf8)!)
  exit(code)
}

let usage = """
  usage: agent24-documents-pdfkit parse <path> <media-type>
         agent24-documents-pdfkit render <path> <media-type> <page> <scale> <max-bytes> [x0,y0,x1,y1]
  """
let args = CommandLine.arguments
let command = args.count > 1 ? args[1] : ""
if !(command == "parse" && args.count == 4) && !(command == "render" && (7...8).contains(args.count)) {
  fail(64, usage)
}
// Made absolute: PDFKit does not resolve a relative file URL.
let url = URL(
  fileURLWithPath: args[2],
  relativeTo: URL(fileURLWithPath: FileManager.default.currentDirectoryPath, isDirectory: true)
).standardizedFileURL
do {
  let media = args[3]
  guard ["application/pdf", "image/jpeg", "image/png"].contains(media) else {
    throw Failure.unsupported("media type \(media)")
  }
  guard try sniff(url) == media else { throw Failure.unsupported("the file is not \(media)") }
  if command == "render" {
    try FileHandle.standardOutput.write(contentsOf: try renderCommand(url, media: media, args: Array(args[4...])))
    exit(0)
  }
  let (pages, unparsed) = media == "application/pdf" ? try readPDF(url) : try readImage(url)
  let v = ProcessInfo.processInfo.operatingSystemVersion
  let os = "\(v.majorVersion).\(v.minorVersion).\(v.patchVersion)"
  // Encoded whole before anything is written. A failed write is exit 3, and
  // the OS reads stdout only after exit 0.
  let json = try JSONEncoder().encode(Output(protocol: 1, os_version: os, pages: pages, unparsed: unparsed))
  try FileHandle.standardOutput.write(contentsOf: json)
  exit(0)
} catch Failure.unsupported(let why) {
  fail(2, why)
} catch Failure.failed(let why) {
  fail(3, why)
} catch Failure.noPage {
  fail(4, "no such page")
} catch Failure.offPage {
  fail(5, "the region is empty or off the page")
} catch Failure.tooLarge {
  fail(6, "too large even at the smallest scale")
} catch {
  fail(3, error.localizedDescription)
}
