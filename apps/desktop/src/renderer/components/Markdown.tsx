// A small Markdown renderer for assistant replies: headings, paragraphs,
// bullet and numbered lists, fenced code, **bold**, *italic* and `code`.
// It builds React elements only (never HTML strings), so a reply cannot
// inject markup; anything it does not know is shown as plain text.

import { Fragment, type ReactNode } from 'react'

/** `**bold**`, `*italic*` / `_italic_`, `` `code` `` inside one line. */
export function inline(text: string, key = 'i'): ReactNode[] {
  const out: ReactNode[] = []
  const pattern = /(`[^`]+`)|(\*\*[^*]+\*\*)|(__[^_]+__)|(\*[^*\s][^*]*\*)|(_[^_\s][^_]*_)/g
  let last = 0
  let n = 0
  for (const m of text.matchAll(pattern)) {
    const at = m.index ?? 0
    if (at > last) out.push(text.slice(last, at))
    const s = m[0]
    const k = `${key}-${n++}`
    if (m[1]) out.push(<code key={k}>{s.slice(1, -1)}</code>)
    else if (m[2] || m[3]) out.push(<strong key={k}>{s.slice(2, -2)}</strong>)
    else out.push(<em key={k}>{s.slice(1, -1)}</em>)
    last = at + s.length
  }
  if (last < text.length) out.push(text.slice(last))
  return out
}

const BULLET = /^\s*[-*•+]\s+(.*)$/
const NUMBERED = /^\s*(\d+)[.)]\s+(.*)$/
const HEADING = /^(#{1,6})\s+(.*)$/

export function Markdown({ text }: { text: string }) {
  const lines = text.replace(/\r\n?/g, '\n').split('\n')
  const blocks: ReactNode[] = []
  let i = 0
  let key = 0
  while (i < lines.length) {
    const line = lines[i]
    if (line.trim() === '') { i++; continue }
    if (line.trimStart().startsWith('```')) {
      const body: string[] = []
      i++
      while (i < lines.length && !lines[i].trimStart().startsWith('```')) body.push(lines[i++])
      i++
      blocks.push(<pre key={key++} className="md-pre"><code>{body.join('\n')}</code></pre>)
      continue
    }
    const h = HEADING.exec(line)
    if (h) {
      blocks.push(<div key={key++} className={`md-h md-h${h[1].length}`}>{inline(h[2], `h${key}`)}</div>)
      i++
      continue
    }
    if (BULLET.test(line) || NUMBERED.test(line)) {
      const numbered = !BULLET.test(line)
      const items: ReactNode[] = []
      const start = numbered ? Number(NUMBERED.exec(line)?.[1] ?? 1) : 1
      while (i < lines.length) {
        const m = numbered ? NUMBERED.exec(lines[i]) : BULLET.exec(lines[i])
        if (!m) break
        items.push(<li key={items.length}>{inline(numbered ? m[2] : m[1], `l${key}-${items.length}`)}</li>)
        i++
      }
      blocks.push(numbered
        ? <ol key={key++} className="md-list" start={start}>{items}</ol>
        : <ul key={key++} className="md-list">{items}</ul>)
      continue
    }
    const para: string[] = []
    while (i < lines.length && lines[i].trim() !== '' && !HEADING.test(lines[i]) && !BULLET.test(lines[i])
      && !NUMBERED.test(lines[i]) && !lines[i].trimStart().startsWith('```')) para.push(lines[i++])
    blocks.push(
      <p key={key++} className="md-p">
        {para.map((l, j) => <Fragment key={j}>{j > 0 && <br />}{inline(l, `p${key}-${j}`)}</Fragment>)}
      </p>,
    )
  }
  return <div className="md">{blocks}</div>
}
