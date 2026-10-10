# Slice-1 read engine: agent24-documents-pdfkit

Slice 1 reads PDF, JPEG and PNG with the system's PDFKit and Vision, through a small out-of-process helper, `agent24-documents-pdfkit` ([ADR-DOC-01](adr/ADR-DOC-01-placement-and-integration.md) D6 amendment, [ADR-DOC-02](adr/ADR-DOC-02-operation-contract.md) §3.1). It is built from [`rust/apps/agent24-documents/engines/pdfkit/`](../../rust/apps/agent24-documents/engines/pdfkit/) with the Xcode command line tools, and shipped next to `bin/agent24-documents`; nothing is downloaded.

```sh
rust/apps/agent24-documents/engines/pdfkit/build.sh <dir>     # writes <dir>/agent24-documents-pdfkit
<dir>/agent24-documents-pdfkit parse <file> application/pdf   # one JSON object on stdout
<dir>/agent24-documents-pdfkit render <file> application/pdf <page> <scale> <max-bytes> [x0,y0,x1,y1]
                                                              # one JSON line, then a PNG
```

- It reads one file per run and writes the pages' lines, in reading order, as text plus a rectangle in CropBox points, origin top left of the page as displayed. It also lists the regions it could not read. It writes no files and uses no network. Its output counts only on exit 0.
- Vision OCR reads every page that has no text, or that draws an image anywhere (including inside form XObjects, and inline images). Mixed pages are put back in reading order. A page it cannot render or recognise is listed as unparsed, not treated as empty. Renders stay within 40 megapixels. JPEG orientation (EXIF) is applied. An image over 100 megapixels is not decoded (exit 3): a small file can hold a huge image.
- Text is kept as read, except for these clean-ups:
  - typographic ligatures (U+FB00–U+FB06) become the letters they stand for;
  - a selection that lies within an earlier line, and says nothing that line does not, is dropped, because that is text drawn twice in place (a fake bold);
  - a selection that spans a line break becomes one line per part, all sharing its rectangle.
- `render` draws one page, or a region of it, on white, with its annotations, in the frame `parse` reports lines in (ADR-DOC-02 §4):
  - **Frame**: CropBox points (the CropBox within the MediaBox, as `parse` takes it), origin top left of the page as displayed. For a JPEG or PNG, its pixels after EXIF orientation. A region is exactly four numbers, corners `x0,y0,x1,y1`, like a line's rectangle.
  - **Region**: one that lies partly off the page is clipped to it; its top-left corner stays where it was asked for. One that is empty or wholly off the page is exit 5.
  - **Scale**: it starts at the scale asked for (0.25 to 4), or lower if the image would pass 40 megapixels. While the PNG is larger than `<max-bytes>`, it renders again at a scale lowered by how much it was over, and by at least 10%; the eighth try is at 0.25. Exit 6 means that 0.25 was tried and is still too large, or that the region passes 40 megapixels even at 0.25. Either way, the remedy is a smaller region.
  - **Output**: a JSON line `{"protocol":1,"scale":…,"width":…,"height":…}` (the scale used, in steps of 0.001; the PNG's size in pixels), then the PNG. A page past the last is exit 4.
- Exit codes: 0 read, possibly in part; 2 not a format it reads (judged by the file's first bytes); 3 a file of that format that could not be read, for example a missing, unreadable, encrypted or corrupt file (stderr says why); 64 usage; `render` adds 4, 5 and 6 above.
- Signing and notarisation go with the OS package's release (jason). Development builds are unsigned.
