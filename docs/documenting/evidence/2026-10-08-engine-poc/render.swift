import PDFKit
import AppKit
// usage: swift render.swift in.pdf outprefix  -> outprefix-N.png, and prints page count + text
let a = CommandLine.arguments
guard let doc = PDFDocument(url: URL(fileURLWithPath: a[1])) else { print("open failed"); exit(1) }
print("pages:", doc.pageCount)
for i in 0..<doc.pageCount {
  let page = doc.page(at: i)!
  let r = page.bounds(for: .mediaBox)
  let img = NSImage(size: NSSize(width: r.width * 1.3, height: r.height * 1.3))
  img.lockFocus(); NSColor.white.set(); NSRect(origin: .zero, size: img.size).fill()
  let ctx = NSGraphicsContext.current!.cgContext; ctx.scaleBy(x: 1.3, y: 1.3); page.draw(with: .mediaBox, to: ctx); img.unlockFocus()
  let rep = NSBitmapImageRep(data: img.tiffRepresentation!)!
  try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: "\(a[2])-\(i+1).png"))
  if a.count > 3 { print("--- page \(i+1)\n" + (page.string ?? "")) }
}
