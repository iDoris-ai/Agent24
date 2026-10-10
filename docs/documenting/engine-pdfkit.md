# Slice-1 read engine: agent24-documents-pdfkit

Slice 1 reads PDF, JPEG and PNG with the system's PDFKit and Vision, through a small out-of-process helper, `agent24-documents-pdfkit` ([ADR-DOC-01](adr/ADR-DOC-01-placement-and-integration.md) D6 amendment, [ADR-DOC-02](adr/ADR-DOC-02-operation-contract.md) §3.1). It is built from [`rust/apps/agent24-documents/engines/pdfkit/main.swift`](../../rust/apps/agent24-documents/engines/pdfkit/main.swift) with the Xcode command line tools, and shipped next to `bin/agent24-documents`; nothing is downloaded.

```sh
rust/apps/agent24-documents/engines/pdfkit/build.sh <dir>     # writes <dir>/agent24-documents-pdfkit
<dir>/agent24-documents-pdfkit parse <file> application/pdf   # one JSON object on stdout
```

- It reads one file per run and writes the pages' lines, in reading order, as text plus a rectangle in CropBox points, origin top left of the page as displayed. It also lists the regions it could not read. It writes no files and uses no network. Its output counts only on exit 0.
- Vision OCR reads every page that has no text, or that draws an image anywhere (including inside form XObjects, and inline images). Mixed pages are put back in reading order. A page it cannot render or recognise is listed as unparsed, not treated as empty. Renders stay within 40 megapixels. JPEG orientation (EXIF) is applied.
- Text is kept as read, except for these clean-ups:
  - typographic ligatures (U+FB00–U+FB06) become the letters they stand for;
  - a selection that lies within an earlier line, and says nothing that line does not, is dropped, because that is text drawn twice in place (a fake bold);
  - a selection that spans a line break becomes one line per part, all sharing its rectangle.
- Exit codes: 0 read, possibly in part; 2 not a format it reads (judged by the file's first bytes); 3 a file of that format that could not be read, for example a missing, unreadable, encrypted or corrupt file (stderr says why); 64 usage.
- Signing and notarisation go with the OS package's release (jason). Development builds are unsigned.
