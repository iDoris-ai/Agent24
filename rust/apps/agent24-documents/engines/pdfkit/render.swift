// `render`: one page, or a region of it, as a PNG of at most a given size
// (ADR-DOC-02 §4). The frame is the one `parse` reports lines in: CropBox
// points, origin top left of the page as displayed; for a JPEG or PNG, its
// pixels after EXIF orientation.

import CoreImage
import Foundation
import PDFKit
import UniformTypeIdentifiers

/// Scales a render may use: the contract's range, in steps of 1/1000 so the
/// scale reported is the one used.
let minScale = 0.25, maxScale = 4.0

/// `s` in steps of 0.001, never above it: s * 1000 can round up to a whole
/// number (0.28099999999999997 to 281), and the scale said must not pass the
/// one asked for.
func stepped(_ s: Double) -> Double {
  let q = (s * 1000).rounded(.down) / 1000
  return q > s ? ((s * 1000).rounded(.down) - 1) / 1000 : q
}

/// What `render` writes before the PNG, as one JSON line.
struct Rendered: Encodable { let `protocol`: Int; let scale: Double; let width: Int; let height: Int }

/// A page's displayed size, and how to draw it at a scale into a context
/// whose origin is a point of the displayed page.
struct Source {
  let size: CGSize
  let draw: (CGContext, CGRect, Double) -> Void
}

/// Page space (origin bottom left, unrotated) to the displayed page (origin
/// top left, after /Rotate): what `displayed` does to a rectangle.
func display(crop: CGRect, rotation: Int) -> CGAffineTransform {
  let (w, h) = (crop.width, crop.height)
  // Into the CropBox, origin top left: (x, y) = (X - minX, h - (Y - minY)).
  let unrotated = CGAffineTransform(a: 1, b: 0, c: 0, d: -1, tx: -crop.minX, ty: h + crop.minY)
  let turn: CGAffineTransform
  switch rotation {
  case 90: turn = CGAffineTransform(a: 0, b: 1, c: -1, d: 0, tx: h, ty: 0)  // (h - y, x)
  case 180: turn = CGAffineTransform(a: -1, b: 0, c: 0, d: -1, tx: w, ty: h)  // (w - x, h - y)
  case 270: turn = CGAffineTransform(a: 0, b: -1, c: 1, d: 0, tx: 0, ty: w)  // (y, w - x)
  default: turn = .identity
  }
  return unrotated.concatenating(turn)
}

func pdfSource(_ url: URL, page number: Int) throws -> Source {
  guard let doc = PDFDocument(url: url) else { throw Failure.failed("PDFKit cannot open this PDF") }
  if doc.isLocked { throw Failure.failed("the PDF is encrypted") }
  guard number <= doc.pageCount else { throw Failure.noPage }
  guard let page = doc.page(at: number - 1) else { throw Failure.failed("page \(number) cannot be opened") }
  // The CropBox as parse takes it: within the MediaBox.
  let crop = page.bounds(for: .cropBox)
  let rotation = ((page.rotation % 360) + 360) % 360
  let size = rotation == 90 || rotation == 270 ? CGSize(width: crop.height, height: crop.width) : crop.size
  return Source(size: size) { ctx, region, scale in
    // Displayed point d lands at pixel ((d - region.origin) * scale), counted
    // from the top; the context counts from the bottom.
    let pixels = CGAffineTransform(a: scale, b: 0, c: 0, d: -scale,
      tx: -region.minX * scale, ty: Double(ctx.height) + region.minY * scale)
    ctx.concatenate(display(crop: crop, rotation: rotation).concatenating(pixels))
    ctx.clip(to: crop)
    // PDFKit draws the page with its annotations, turning it as displayed
    // first; that turn is undone, so page space meets the transform above.
    // The page holds its document weakly: the closure keeps it.
    ctx.concatenate(page.transform(for: .mediaBox).inverted())
    withExtendedLifetime(doc) { page.draw(with: .mediaBox, to: ctx) }
  }
}

func imageSource(_ url: URL, page number: Int) throws -> Source {
  guard number == 1 else { throw Failure.noPage }
  let (image, raw) = try decoded(url)
  // Core Image applies the orientation; its origin is bottom left.
  let oriented = CIImage(cgImage: image).oriented(forExifOrientation: Int32(raw))
  let shown = oriented.transformed(by: CGAffineTransform(translationX: -oriented.extent.minX, y: -oriented.extent.minY))
  let shownSize = shown.extent.size
  return Source(size: shownSize) { ctx, region, scale in
    let flipped = CGRect(x: region.minX, y: shownSize.height - region.maxY, width: region.width, height: region.height)
    let cropped = shown.cropped(to: flipped)
      .transformed(by: CGAffineTransform(translationX: -flipped.minX, y: -flipped.minY))
      .transformed(by: CGAffineTransform(scaleX: scale, y: scale))
    // Top-aligned, as the PDF is: the context may be a part pixel taller.
    let top = CGRect(x: 0, y: Double(ctx.height) - cropped.extent.height,
      width: cropped.extent.width, height: cropped.extent.height)
    CIContext(cgContext: ctx, options: nil).draw(cropped, in: top, from: cropped.extent)
  }
}

/// `region` drawn at `scale`, as a PNG, on white.
/// Pixels for `points` at `scale`: rounded up, at least one.
func pixels(_ points: Double, _ scale: Double) -> Int { max(1, Int((points * scale).rounded(.up))) }

func png(_ source: Source, _ region: CGRect, _ scale: Double) throws -> (Data, Int, Int) {
  let (w, h) = (pixels(region.width, scale), pixels(region.height, scale))
  guard let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
    space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)
  else { throw Failure.failed("no memory for a \(w)x\(h) render") }
  ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
  ctx.fill(CGRect(x: 0, y: 0, width: w, height: h))
  source.draw(ctx, region, scale)
  let data = NSMutableData()
  guard let image = ctx.makeImage(),
    let out = CGImageDestinationCreateWithData(data, UTType.png.identifier as CFString, 1, nil)
  else { throw Failure.failed("the render cannot be encoded") }
  CGImageDestinationAddImage(out, image, nil)
  guard CGImageDestinationFinalize(out) else { throw Failure.failed("the render cannot be encoded") }
  return (data as Data, w, h)
}

/// The region asked for, clipped to the page; none is the whole page. Its
/// top-left corner must be on the page (0 or more, like a line's rectangle),
/// so clipping only trims the right and bottom and the corner stays where it
/// was asked for: pixels map back to the page from it. One that is not four
/// numbers, is empty, starts left of or above the page, or lies wholly off
/// it is refused (exit 5): only here is the page's size known.
func clipped(_ asked: String?, to size: CGSize) throws -> CGRect {
  let page = CGRect(origin: .zero, size: size)
  guard let asked else { return page }
  let fields = asked.split(separator: ",", omittingEmptySubsequences: false)
  let v = fields.compactMap { Double($0) }
  guard fields.count == 4, v.count == 4, v.allSatisfy({ $0.isFinite && $0 >= 0 }), v[0] < v[2], v[1] < v[3]
  else {
    throw Failure.offPage
  }
  let r = CGRect(x: v[0], y: v[1], width: v[2] - v[0], height: v[3] - v[1]).intersection(page)
  guard !r.isNull, r.width > 0, r.height > 0 else { throw Failure.offPage }
  return r
}

/// A PNG within `maxBytes` and the pixel budget, from the scale asked for
/// down: each try that is too large sets the next by how much it was over,
/// at least 10% lower; the eighth is 0.25. It never searches back up. So it
/// is too large (exit 6) only when 0.25 was tried, or is over the budget.
func render(_ source: Source, region: CGRect, asked: Double, maxBytes: Int) throws -> (Rendered, Data) {
  let area = Double(region.width * region.height)
  var scale = min(asked, (maxPixels / area).squareRoot())
  for tries in 1... {
    scale = max(minScale, stepped(scale))
    // The bitmap's real size: each side rounded up, as `png` makes it.
    guard Double(pixels(region.width, scale) * pixels(region.height, scale)) <= maxPixels else {
      throw Failure.tooLarge
    }
    let (data, w, h) = try autoreleasepool { try png(source, region, scale) }
    if data.count <= maxBytes { return (Rendered(protocol: 1, scale: scale, width: w, height: h), data) }
    if scale == minScale { throw Failure.tooLarge }
    let over = (Double(maxBytes) / Double(data.count)).squareRoot() * 0.95
    scale = tries >= 7 ? minScale : scale * min(0.9, over)
  }
  throw Failure.tooLarge
}

func renderCommand(_ url: URL, media: String, args: [String]) throws -> Data {
  guard let page = Int(args[0]), page >= 1, let scale = Double(args[1]), scale >= minScale, scale <= maxScale,
    let maxBytes = Int(args[2]), maxBytes > 0
  else { fail(64, usage) }
  let source = media == "application/pdf" ? try pdfSource(url, page: page) : try imageSource(url, page: page)
  let region = try clipped(args.count > 3 ? args[3] : nil, to: source.size)
  let (head, png) = try render(source, region: region, asked: scale, maxBytes: maxBytes)
  var out = try JSONEncoder().encode(head)
  out.append(0x0A)
  out.append(png)
  return out
}
