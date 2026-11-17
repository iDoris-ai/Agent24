// `compose <out.pdf> <in.pdf> <pages> [<in.pdf> <pages> …]` (D2 拆分合并 PDF):
// a new PDF of the given pages of the given files, in that order, written
// to <out.pdf>. <pages> is `all` or 1-based pages and ranges, `1-3,5`.
// Splitting is one input with some pages; merging is several with `all`.
// Writes one JSON line, `{"pages":N}`. Exit 3: an input that could not be
// read or the output not written; 4: a page past an input's last; 64: usage.

import Foundation
import PDFKit

/// `all`, or `1-3,5`: 1-based, each page at most once, in the order given.
func pageList(_ spec: String, count: Int) throws -> [Int] {
  if spec == "all" { return Array(1...max(count, 1)).filter { $0 <= count } }
  var pages: [Int] = []
  for part in spec.split(separator: ",", omittingEmptySubsequences: false) {
    let ends = part.split(separator: "-", omittingEmptySubsequences: false).map { Int($0.trimmingCharacters(in: .whitespaces)) }
    guard (1...2).contains(ends.count), let first = ends.first ?? nil, let last = ends.last ?? nil, first >= 1, first <= last else {
      fail(64, "pages must be `all` or like 1-3,5")
    }
    guard last <= count else { throw Failure.noPage }
    pages.append(contentsOf: first...last)
  }
  return pages
}

func composeCommand(_ args: [String]) throws -> Data {
  guard args.count >= 3, args.count % 2 == 1 else {
    fail(64, "usage: agent24-documents-pdfkit compose <out.pdf> <in.pdf> <pages> [<in.pdf> <pages> …]")
  }
  let cwd = URL(fileURLWithPath: FileManager.default.currentDirectoryPath, isDirectory: true)
  let out = URL(fileURLWithPath: args[0], relativeTo: cwd).standardizedFileURL
  let result = PDFDocument()
  var index = 1
  while index < args.count {
    let input = URL(fileURLWithPath: args[index], relativeTo: cwd).standardizedFileURL
    guard try sniff(input) == "application/pdf", let doc = PDFDocument(url: input) else {
      throw Failure.failed("not a readable PDF: input \((index + 1) / 2)")
    }
    if doc.isLocked { throw Failure.failed("input \((index + 1) / 2) is password protected") }
    for p in try pageList(args[index + 1], count: doc.pageCount) {
      guard let page = doc.page(at: p - 1)?.copy() as? PDFPage else { throw Failure.noPage }
      result.insert(page, at: result.pageCount)
    }
    index += 2
  }
  guard result.pageCount > 0 else { throw Failure.failed("no pages") }
  guard result.write(to: out) else { throw Failure.failed("could not write the output") }
  return try JSONEncoder().encode(["pages": result.pageCount]) + Data("\n".utf8)
}
