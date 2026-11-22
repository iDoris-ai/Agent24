// UI polish 01: the native box before an approval, in the user's words.
//
// The text comes only from the daemon's own record of the approval (read by
// main, by id) and from what main itself reads of the documents it names;
// nothing the page sends ends up here except its reason, marked as a note.
// No approval, run or document ids and no JSON are shown: the box says what
// the assistant will do, to which document, and what it changes.

import { mt, type MainMessageKey } from './main-i18n'

export interface ApprovalRecord { summary?: unknown; payload?: unknown }

/** What main reads of a document the approval names; null when it cannot. */
export interface DocumentLookups {
  title: (id: string) => Promise<string | null>
  paragraphs: (id: string) => Promise<ReadonlyMap<number, string> | null>
}

export type ApprovalDialog =
  | { ok: true; message: string; detail: string }
  /** A patch whose edits cannot be read: it is not offered (PR-Daemon #1986 A). */
  | { ok: false }

/** Edits listed in the box; the count is always the record's own. */
export const SHOWN_EDITS = 5
const EXCERPT = 60

const TOOL_KEYS: Record<string, MainMessageKey> = {
  'documents.list': 'approval.tool.list', 'documents.get': 'approval.tool.get',
  'documents.paragraphs': 'approval.tool.paragraphs', 'documents.fields': 'approval.tool.fields',
  shell_exec: 'approval.tool.exec', fs_write: 'approval.tool.write', http_fetch: 'approval.tool.fetch',
}

/** UI polish 10 (General 0.6.96): read-only documents tools: nothing to show
 *  past the document they read, and they change nothing. */
const READ_ONLY = new Set(['documents.list', 'documents.get', 'documents.paragraphs', 'documents.fields'])

/** A tool's input as lines, key by key, nested arrays and objects indented;
 *  never JSON (the native box has no 「详情」 to fold it into). */
export function inputLines(input: Record<string, unknown>): string[] {
  const lines: string[] = []
  const walk = (v: unknown, pad: string, head: string) => {
    if (v === null || v === undefined || v === '' || (typeof v === 'object' && Object.keys(v as object).length === 0)) {
      lines.push(`${head}${mt('approval.listEmpty')}`)
    } else if (typeof v !== 'object') lines.push(`${head}${String(v)}`)
    else if (Array.isArray(v) && v.every(x => x === null || typeof x !== 'object')) {
      lines.push(head.trimEnd())
      v.forEach((x, i) => lines.push(`${pad}  ${i + 1}. ${String(x)}`))
    } else {
      lines.push(head.trimEnd())
      const entries = Array.isArray(v) ? v.map((x, i) => [`${i + 1}.`, x] as const)
        : Object.entries(v as Record<string, unknown>).map(([k, x]) => [mt('approval.listKey', { key: k }), x] as const)
      for (const [label, x] of entries) walk(x, `${pad}  `, `${pad}  ${label}${Array.isArray(v) ? ' ' : ''}`)
    }
  }
  for (const [k, v] of Object.entries(input)) walk(v, '', mt('approval.listKey', { key: k }))
  return lines
}

const excerpt = (s: string): string => (s.length > EXCERPT ? `${s.slice(0, EXCERPT)}…` : s)
/** PR-Daemon #1995 L1: the original and the new text, each cut to the same
 *  window around the first character where they differ, so a change past the
 *  60th character is not hidden behind two identical openings. */
export function changeExcerpts(from: string, to: string): [string, string] {
  let same = 0
  while (same < from.length && same < to.length && from[same] === to[same]) same++
  const start = same < EXCERPT - 20 ? 0 : same - 20
  const cut = (s: string) => `${start > 0 ? '…' : ''}${s.slice(start, start + EXCERPT)}${s.length > start + EXCERPT ? '…' : ''}`
  return [cut(from), cut(to)]
}
const str = (v: unknown): string => (typeof v === 'string' ? v : '')
const isDocId = (ref: string): boolean => /^doc_[0-9A-Za-z]+$/.test(ref)

/** The tool's name, as the daemon writes it before `: ` in the summary. */
export function toolOf(summary: unknown): string {
  const s = str(summary)
  const cut = s.indexOf(': ')
  return cut > 0 ? s.slice(0, cut) : s
}

/** A document the model named, by its title: read by id, or the name it used. */
async function titleOf(ref: string, lookups: DocumentLookups): Promise<string> {
  if (isDocId(ref)) return (await lookups.title(ref).catch(() => null)) || mt('approval.someDocument')
  return ref.replace(/^《|》$/g, '') || mt('approval.someDocument')
}

export async function approvalDialog(
  record: ApprovalRecord,
  decision: string,
  decisionLabel: string,
  reason: string | undefined,
  lookups: DocumentLookups,
  clip: (text: string, max: number) => string,
): Promise<ApprovalDialog> {
  const tool = toolOf(record.summary)
  const args = record.payload && typeof record.payload === 'object' && !Array.isArray(record.payload)
    ? record.payload as Record<string, unknown>
    : {}
  const lines: string[] = []
  let message: string
  if (tool === 'documents.patch') {
    // Never a summary that looks whole when the record's edits are not there.
    if (!Array.isArray(args['edits'])) return { ok: false }
    const edits = args['edits'] as Array<Record<string, unknown>>
    const ref = str(args['document_id'])
    const [doc, originals] = await Promise.all([
      titleOf(ref, lookups),
      isDocId(ref) ? lookups.paragraphs(ref).catch(() => null) : Promise.resolve(null),
    ])
    message = mt('approval.title.patch', { doc })
    const tracked = edits.some(e => e && e['tracked'] === true)
    lines.push(mt(tracked ? 'approval.patch.tracked' : 'approval.patch.plain', { n: edits.length }))
    if (edits.length > SHOWN_EDITS) lines.push(mt('approval.patch.more', { n: edits.length, shown: SHOWN_EDITS }))
    for (const e of edits.slice(0, SHOWN_EDITS)) {
      const index = e && typeof e['index'] === 'number' ? e['index'] : null
      const from = index === null ? undefined : originals?.get(index)
      const n = index === null ? '?' : index + 1
      const text = str(e?.['text'])
      if (from === undefined) lines.push(mt('approval.patch.editUnread', { n, to: excerpt(text) }))
      else {
        const [was, now] = changeExcerpts(from, text)
        lines.push(mt('approval.patch.edit', { n, from: was, to: now }))
      }
    }
    lines.push(mt('approval.unchanged'))
  } else if (tool === 'documents.fill') {
    message = mt('approval.title.fill', { doc: await titleOf(str(args['document_id']), lookups) })
    const values = args['values'] && typeof args['values'] === 'object'
      ? Object.entries(args['values'] as Record<string, unknown>).map(([k, v]) => `${k}=${str(v)}`).join('，')
      : ''
    lines.push(mt('approval.fill.values', { values: clip(values, 300) }), mt('approval.unchanged'))
  } else if (tool === 'documents.compose' && Array.isArray(args['parts'])) {
    message = mt('approval.title.compose', { title: clip(str(args['title']), 120) || mt('approval.newDocument') })
    const parts = await Promise.all((args['parts'] as Array<Record<string, unknown>>).map(async p => {
      const doc = await titleOf(str(p?.['document_id']), lookups)
      const pages = str(p?.['pages'])
      return pages ? mt('approval.compose.part', { doc, pages }) : mt('approval.compose.partAll', { doc })
    }))
    lines.push(mt('approval.compose.from', { parts: clip(parts.join(' + '), 400) }), mt('approval.unchanged'))
  } else {
    // UI polish 10: the document by its title; the rest as a list, never
    // JSON; a read-only documents tool says only that it changes nothing.
    const key = TOOL_KEYS[tool]
    const action = key ? mt(key) : clip(tool, 60)
    const ref = str(args['document_id'])
    message = ref
      ? mt('approval.title.toolDoc', { action, doc: await titleOf(ref, lookups) })
      : mt('approval.title.tool', { action })
    if (READ_ONLY.has(tool)) lines.push(mt('approval.readOnly'))
    else {
      const rest = Object.fromEntries(Object.entries(args).filter(([k]) => !(ref && k === 'document_id')))
      const cut = str(record.summary).indexOf(': ')
      // No payload (an older daemon): the daemon's summary is all there is.
      const said = Object.keys(rest).length > 0 ? inputLines(rest).join('\n')
        : Object.keys(args).length === 0 && cut > 0 ? str(record.summary).slice(cut + 2) : ''
      if (said) lines.push(mt('approval.content', { value: clip(said, 600) }))
      lines.push(mt('approval.warning'))
    }
  }
  if (decision !== 'approve') lines.unshift(mt('approval.decision', { decision: decisionLabel }))
  if (reason) lines.push(mt('approval.note', { value: clip(reason, 200) }))
  return { ok: true, message, detail: lines.join('\n') }
}
