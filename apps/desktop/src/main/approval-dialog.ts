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
  /** The library's documents, to find one the model named by its file name. */
  list: () => Promise<ReadonlyArray<{ id: string; title: string }> | null>
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

/** PR-Daemon #2006 B1: text the model controls never makes a line of its
 *  own in the box. Line breaks, other control characters and the characters
 *  that reorder text are shown as escapes (`\n`, `\u{202e}`), as JSON did. */
// eslint-disable-next-line no-control-regex
const HIDDEN = /[\u0000-\u001f\u007f-\u009f\u2028\u2029\u202a-\u202e\u2066-\u2069]/g
export const visible = (s: string): string => s.replace(HIDDEN, (c) =>
  c === '\n' ? '\\n' : c === '\r' ? '\\r' : c === '\t' ? '\\t' : `\\u{${c.codePointAt(0)!.toString(16)}}`)
/** `mt` with every string parameter made visible. */
const say = (key: MainMessageKey, params: Record<string, string | number> = {}): string =>
  mt(key, Object.fromEntries(Object.entries(params).map(([k, v]) => [k, typeof v === 'string' ? visible(v) : v])))

/** Nesting shown before 「…」 (PR-Daemon #2006 L1). */
const MAX_DEPTH = 6

/** A tool's input as lines, key by key, nested arrays and objects indented;
 *  never JSON (the native box has no 「详情」 to fold it into). Keys and
 *  values are made visible: none of them starts a line of its own. */
export function inputLines(input: Record<string, unknown>): string[] {
  const lines: string[] = []
  const walk = (v: unknown, pad: string, head: string, depth: number) => {
    if (v === null || v === undefined || v === '' || (typeof v === 'object' && Object.keys(v as object).length === 0)) {
      lines.push(`${head}${mt('approval.listEmpty')}`)
    } else if (typeof v !== 'object') lines.push(`${head}${visible(String(v))}`)
    else if (depth >= MAX_DEPTH) lines.push(`${head}…`)
    else if (Array.isArray(v) && v.every(x => x === null || typeof x !== 'object')) {
      lines.push(head.trimEnd())
      v.forEach((x, i) => lines.push(`${pad}  ${i + 1}. ${visible(String(x))}`))
    } else {
      lines.push(head.trimEnd())
      const entries = Array.isArray(v) ? v.map((x, i) => [`${i + 1}.`, x] as const)
        : Object.entries(v as Record<string, unknown>).map(([k, x]) => [say('approval.listKey', { key: k }), x] as const)
      for (const [label, x] of entries) walk(x, `${pad}  `, `${pad}  ${label}${Array.isArray(v) ? ' ' : ''}`, depth + 1)
    }
  }
  for (const [k, v] of Object.entries(input)) walk(v, '', say('approval.listKey', { key: k }), 1)
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
/** A document id as the module tells one from a name (agent24-documents
 *  `is_id`): `doc_` and 26 Crockford base32 characters, the first 0–7.
 *  Anything else the module looks up by title, so the box does too. */
const isDocId = (ref: string): boolean => /^doc_[0-7][0-9A-HJKMNP-TV-Z]{25}$/.test(ref)
/** The tools whose `document_id` the module looks up by title (`named_by_title`). */
const NAMED_BY_TITLE = new Set(['documents.get', 'documents.paragraphs', 'documents.patch', 'documents.fields', 'documents.fill', 'documents.compose'])

/** The library's documents from a `documents list` answer, or null when it
 *  is not the whole library (PR-Daemon #2006 N1): the module matches a name
 *  against every document, so a match in one page may not be its match. */
export function wholeLibrary(data: unknown): Array<{ id: string; title: string }> | null {
  const page = data as { documents?: unknown; next_cursor?: unknown } | null
  if (!Array.isArray(page?.documents) || (typeof page.next_cursor === 'string' && page.next_cursor !== '')) return null
  return page.documents.flatMap((d: { document_id?: unknown; title?: unknown }) =>
    typeof d?.document_id === 'string' && typeof d.title === 'string' ? [{ id: d.document_id, title: d.title }] : [])
}

/** The tool's name, as the daemon writes it before `: ` in the summary. */
export function toolOf(summary: unknown): string {
  const s = str(summary)
  const cut = s.indexOf(': ')
  return cut > 0 ? s.slice(0, cut) : s
}

/** A title or a name for it as the documents module compares them
 *  (agent24-documents `plain_title`): no case, quotes, space or extension. */
export function plainTitle(raw: string): string {
  const s = raw.trim().replace(/^[《》「」"“”]+|[《》「」"“”]+$/g, '').trim().toLowerCase()
  const dot = s.lastIndexOf('.')
  const ext = dot > 0 ? s.slice(dot + 1) : ''
  return dot > 0 && ext.length >= 1 && ext.length <= 5 && /^[a-z0-9]+$/.test(ext) ? s.slice(0, dot) : s
}

/** General 0.6.96: a model names a document by its file name as often as by
 *  its id, and the module takes either (`named_by_title`: the one equal title,
 *  else the one containing it). The same here, so the box reads the document
 *  the module will write; none or several: not found, as there. */
export function namedDocument(docs: ReadonlyArray<{ id: string; title: string }>, name: string): { id: string; title: string } | null {
  const wanted = plainTitle(name)
  if (!wanted) return null
  const only = (hits: ReadonlyArray<{ id: string; title: string }>) => (hits.length === 1 ? hits[0]! : null)
  return only(docs.filter(d => plainTitle(d.title) === wanted)) ?? only(docs.filter(d => plainTitle(d.title).includes(wanted)))
}

/** A document the model named: its id when main can find it, and its title.
 *  A name is looked up only for the tools the module looks it up for
 *  (`byName`, PR-Daemon #2006 N3); for others the box shows it as written. */
async function docOf(ref: string, lookups: DocumentLookups, byName = true): Promise<{ id: string | null; title: string }> {
  if (isDocId(ref)) return { id: ref, title: (await lookups.title(ref).catch(() => null)) || mt('approval.someDocument') }
  const found = ref && byName ? namedDocument((await lookups.list().catch(() => null)) ?? [], ref) : null
  return found ?? { id: null, title: ref.replace(/^《|》$/g, '') || mt('approval.someDocument') }
}
const titleOf = async (ref: string, lookups: DocumentLookups, byName = true): Promise<string> => (await docOf(ref, lookups, byName)).title

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
    const named = await docOf(ref, lookups)
    const originals = named.id ? await lookups.paragraphs(named.id).catch(() => null) : null
    message = say('approval.title.patch', { doc: named.title })
    const tracked = edits.some(e => e && e['tracked'] === true)
    lines.push(mt(tracked ? 'approval.patch.tracked' : 'approval.patch.plain', { n: edits.length }))
    if (edits.length > SHOWN_EDITS) lines.push(mt('approval.patch.more', { n: edits.length, shown: SHOWN_EDITS }))
    for (const e of edits.slice(0, SHOWN_EDITS)) {
      const index = e && typeof e['index'] === 'number' ? e['index'] : null
      const from = index === null ? undefined : originals?.get(index)
      const n = index === null ? '?' : index + 1
      const text = str(e?.['text'])
      if (from === undefined) lines.push(say('approval.patch.editUnread', { n, to: excerpt(text) }))
      else {
        const [was, now] = changeExcerpts(from, text)
        lines.push(say('approval.patch.edit', { n, from: was, to: now }))
      }
    }
    lines.push(mt('approval.unchanged'))
  } else if (tool === 'documents.fill') {
    message = say('approval.title.fill', { doc: await titleOf(str(args['document_id']), lookups) })
    const values = args['values'] && typeof args['values'] === 'object'
      ? Object.entries(args['values'] as Record<string, unknown>).map(([k, v]) => `${k}=${str(v)}`).join('，')
      : ''
    lines.push(say('approval.fill.values', { values: clip(values, 300) }), mt('approval.unchanged'))
  } else if (tool === 'documents.compose' && Array.isArray(args['parts'])) {
    message = say('approval.title.compose', { title: clip(str(args['title']), 120) || mt('approval.newDocument') })
    const parts = await Promise.all((args['parts'] as Array<Record<string, unknown>>).map(async p => {
      const doc = await titleOf(str(p?.['document_id']), lookups)
      const pages = str(p?.['pages'])
      return pages ? say('approval.compose.part', { doc, pages }) : say('approval.compose.partAll', { doc })
    }))
    lines.push(mt('approval.compose.from', { parts: clip(parts.join(' + '), 400) }), mt('approval.unchanged'))
  } else {
    // UI polish 10: the document by its title; the rest as a list, never
    // JSON; a read-only documents tool says only that it changes nothing.
    const key = TOOL_KEYS[tool]
    const action = key ? mt(key) : clip(tool, 60)
    const ref = str(args['document_id'])
    message = ref
      ? say('approval.title.toolDoc', { action, doc: await titleOf(ref, lookups, NAMED_BY_TITLE.has(tool)) })
      : say('approval.title.tool', { action })
    if (READ_ONLY.has(tool)) lines.push(mt('approval.readOnly'))
    else {
      const rest = Object.fromEntries(Object.entries(args).filter(([k]) => !(ref && k === 'document_id')))
      const cut = str(record.summary).indexOf(': ')
      // No payload (an older daemon): the daemon's summary is all there is.
      const said = Object.keys(rest).length > 0 ? inputLines(rest).join('\n')
        : Object.keys(args).length === 0 && cut > 0 ? visible(str(record.summary).slice(cut + 2)) : ''
      if (said) lines.push(mt('approval.content', { value: clip(said, 600) }))
      lines.push(mt('approval.warning'))
    }
  }
  if (decision !== 'approve') lines.unshift(say('approval.decision', { decision: decisionLabel }))
  if (reason) lines.push(say('approval.note', { value: clip(reason, 200) }))
  return { ok: true, message, detail: lines.join('\n') }
}
