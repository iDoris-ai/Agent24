// Makes the render fixtures: one US Letter page with "MARK" near the top
// left and nothing else, /Rotate 0, 90, 180 and 270; and the same with a
// CropBox that does not start at the MediaBox's origin (crop*.pdf). Run
// from this folder:
//   xcrun swift make.swift
import CoreText
import Foundation
import PDFKit

let box = CGRect(x: 0, y: 0, width: 612, height: 792)
for rotation in [0, 90, 180, 270] {
  let data = NSMutableData()
  var media = box
  let ctx = CGContext(consumer: CGDataConsumer(data: data)!, mediaBox: &media, nil)!
  ctx.beginPDFPage(nil)
  let font = CTFontCreateWithName("Helvetica-Bold" as CFString, 36, nil)
  let text = NSAttributedString(string: "MARK", attributes: [.font: font])
  ctx.textPosition = CGPoint(x: 72, y: 700)
  CTLineDraw(CTLineCreateWithAttributedString(text), ctx)
  ctx.endPDFPage()
  ctx.closePDF()
  let doc = PDFDocument(data: data as Data)!
  doc.page(at: 0)!.rotation = rotation
  doc.write(toFile: "rot\(rotation).pdf")
  // MARK stays inside: it is drawn from (72, 700), 36 pt high.
  doc.page(at: 0)!.setBounds(CGRect(x: 36, y: 48, width: 540, height: 712), for: .cropBox)
  doc.write(toFile: "crop\(rotation).pdf")
}
