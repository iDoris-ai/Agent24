// D4 (PR-Daemon #1915 B1): the translation's DOCX, written here in plain
// code. No LibreOffice runs in this unsandboxed process: the one installed
// for the documents module lives where that module can write, so running it
// here would let a module that was taken over run its code outside SBX-1.
// The PDF is made by the module itself, inside its sandbox (`convert` job).
//
// A DOCX is a zip of three XML parts, stored without compression.

export interface DocxParagraph { text: string; pageBreakBefore: boolean }

const CRC_TABLE = (() => {
  const table = new Uint32Array(256)
  for (let n = 0; n < 256; n++) {
    let c = n
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
    table[n] = c >>> 0
  }
  return table
})()

export function crc32(bytes: Uint8Array): number {
  let c = 0xffffffff
  for (const b of bytes) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8)
  return (c ^ 0xffffffff) >>> 0
}

/** A zip of `files`, each stored (method 0), names in UTF-8. */
export function storedZip(files: ReadonlyArray<{ name: string; data: Buffer }>): Buffer {
  const locals: Buffer[] = []
  const centrals: Buffer[] = []
  let offset = 0
  for (const file of files) {
    const name = Buffer.from(file.name, 'utf8')
    const crc = crc32(file.data)
    const local = Buffer.alloc(30)
    local.writeUInt32LE(0x04034b50, 0)
    local.writeUInt16LE(20, 4)
    local.writeUInt16LE(0x0800, 6)
    local.writeUInt16LE(0, 8)
    local.writeUInt16LE(0, 10)
    local.writeUInt16LE(0x21, 12)
    local.writeUInt32LE(crc, 14)
    local.writeUInt32LE(file.data.length, 18)
    local.writeUInt32LE(file.data.length, 22)
    local.writeUInt16LE(name.length, 26)
    local.writeUInt16LE(0, 28)
    locals.push(local, name, file.data)
    const central = Buffer.alloc(46)
    central.writeUInt32LE(0x02014b50, 0)
    central.writeUInt16LE(20, 4)
    central.writeUInt16LE(20, 6)
    central.writeUInt16LE(0x0800, 8)
    central.writeUInt16LE(0, 10)
    central.writeUInt16LE(0, 12)
    central.writeUInt16LE(0x21, 14)
    central.writeUInt32LE(crc, 16)
    central.writeUInt32LE(file.data.length, 20)
    central.writeUInt32LE(file.data.length, 24)
    central.writeUInt16LE(name.length, 28)
    central.writeUInt32LE(offset, 42)
    centrals.push(central, name)
    offset += 30 + name.length + file.data.length
  }
  const directory = Buffer.concat(centrals)
  const end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0)
  end.writeUInt16LE(files.length, 8)
  end.writeUInt16LE(files.length, 10)
  end.writeUInt32LE(directory.length, 12)
  end.writeUInt32LE(offset, 16)
  return Buffer.concat([...locals, directory, end])
}

function xmlText(s: string): string {
  // Characters XML 1.0 does not allow are dropped; the rest escaped.
  return [...s]
    .filter((ch) => {
      const c = ch.codePointAt(0) ?? 0
      return c === 0x09 || c === 0x0a || c === 0x0d || (c >= 0x20 && c !== 0xfffe && c !== 0xffff)
    })
    .join('')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
}

const CONTENT_TYPES = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
  + '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
  + '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
  + '<Default Extension="xml" ContentType="application/xml"/>'
  + '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>'
  + '</Types>'

const RELS = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
  + '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
  + '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>'
  + '</Relationships>'

/** The paragraphs, in order, a page break where the original page changed. */
export function docxBytes(paragraphs: readonly DocxParagraph[]): Buffer {
  const body = paragraphs.map((p) => {
    const pPr = p.pageBreakBefore ? '<w:pPr><w:pageBreakBefore/></w:pPr>' : ''
    return `<w:p>${pPr}<w:r><w:rPr><w:rFonts w:eastAsia="Noto Sans SC"/></w:rPr><w:t xml:space="preserve">${xmlText(p.text)}</w:t></w:r></w:p>`
  }).join('')
  const document = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
    + '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>'
    + body
    + '<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr>'
    + '</w:body></w:document>'
  return storedZip([
    { name: '[Content_Types].xml', data: Buffer.from(CONTENT_TYPES, 'utf8') },
    { name: '_rels/.rels', data: Buffer.from(RELS, 'utf8') },
    { name: 'word/document.xml', data: Buffer.from(document, 'utf8') },
  ])
}
