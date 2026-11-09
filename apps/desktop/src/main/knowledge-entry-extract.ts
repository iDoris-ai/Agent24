// KB-SOP-01b-2 (#1669): rule table → local model → rule table.
//
// 1. Pre-redaction: every message the user chose is cleared the way "一键脱敏"
//    clears a draft (`clearFindings`: the rule table, then whatever the
//    post-scan still holds back) before anything else sees it. A range that is
//    still not clean after that never reaches the model: the user gets the
//    empty template (`held_back`). Only the user's and the
//    assistant's text is used, never tool results, memories or attachments: a
//    history record holds nothing else.
// 2. The model: the fixed skill (knowledge-entry-skill.ts) over
//    `/api/v1/chat` without `session_id`, so local models only, no memory, no
//    tools. When there is no usable answer the user gets an empty template and
//    is told why; nothing fails silently.
// 3. Post-scan: not here but on every read of a draft (`findings`), since the
//    user can edit it: any hit blocks submission, whatever the model said.

import { createHash } from 'node:crypto'
import type { BackendProxyResponse } from '../shared/ipc-types'
import type { ChatHistoryRecord } from '../shared/chat-history'
import { MAX_ENTRY_TITLE, type EntryKind } from './knowledge-entry-draft'
import { placeholder, postScan, preRedact, type RedactionCategory } from './knowledge-entry-redact'
import { clipUnits, extractRequest, parseModelDraft, wellFormed, type SourceMessage } from './knowledge-entry-skill'

/** Characters of redacted conversation sent to the model, at most. A longer
 *  range is refused rather than cut: a draft of half a range would mislead. */
export const MAX_SOURCE_CHARS = 48_000

/** How the model step went; anything but `drafted` gives the empty template.
 *  `held_back`: the chosen messages could not be cleared, so the model was not asked. */
export type ModelOutcome = 'drafted' | 'no_model' | 'needs_confirmation' | 'busy' | 'failed' | 'unusable' | 'held_back'

export type ChatCall = (body: unknown) => Promise<BackendProxyResponse>

export interface Extraction {
  kind: EntryKind
  title: string
  body: string
  /** Words of the body the model asks the user to look at again. Advisory only. */
  hints: string[]
  model: ModelOutcome
}

export class SourceTooLong extends Error {
  constructor() {
    super('The chosen messages are too long to draft from')
  }
}

/** The chosen messages, cleared; `clean` is false when the post-scan still finds something in one. */
export interface Source {
  messages: SourceMessage[]
  clean: boolean
}

/** Messages `from`..`to` (inclusive) of `record`, cleared. Throws SourceTooLong. */
export function sourceMessages(record: ChatHistoryRecord, from: number, to: number): Source {
  const messages: SourceMessage[] = []
  let clean = true
  let total = 0
  for (const message of record.messages.slice(from, to + 1)) {
    let text: string
    try {
      text = clearFindings(message.content)
      if (postScan(text).length > 0) clean = false
    } catch {
      throw new SourceTooLong()
    }
    total += text.length
    if (total > MAX_SOURCE_CHARS) throw new SourceTooLong()
    messages.push({ role: message.role, text })
  }
  return { messages, clean }
}

/** A title for a draft the model did not title: the conversation's own,
 *  cleared, on one line; `untitled` when nothing is left or it is still not clean. */
export function fallbackTitle(conversationTitle: string, untitled: string): string {
  let title: string
  try {
    title = clearFindings(wellFormed(conversationTitle)).normalize('NFC')
  } catch {
    title = ''
  }
  const line = clipUnits(title.replace(/\s+/g, ' ').trim(), MAX_ENTRY_TITLE).trim()
  return line.length > 0 && postScan(line).length === 0 ? line : untitled
}

function errorCode(res: BackendProxyResponse): string | null {
  const error = (res.data as { error?: { code?: unknown } } | null)?.error
  return typeof error?.code === 'string' ? error.code : null
}

function outcomeOf(res: BackendProxyResponse): ModelOutcome {
  const code = errorCode(res)
  if (code === 'provider_unavailable') return 'no_model'
  // Never confirmed for a draft, as for a title: the user can do that in a chat.
  if (code === 'idoris_headroom_confirmation_required') return 'needs_confirmation'
  if (code === 'idoris_supervisor_busy') return 'busy'
  return 'failed'
}

/** Asks the local model once, and only about a clean source. Never throws: a failure is an outcome. */
export async function extractEntry(source: Source, untitledTitle: string, call: ChatCall): Promise<Extraction> {
  const template = (model: ModelOutcome): Extraction => ({ kind: 'note', title: untitledTitle, body: '', hints: [], model })
  if (!source.clean) return template('held_back')
  let res: BackendProxyResponse
  try {
    res = await call(extractRequest(source.messages))
  } catch {
    return template('failed')
  }
  if (!res || !res.ok) return template(res ? outcomeOf(res) : 'failed')
  const draft = parseModelDraft((res.data as { message?: { content?: unknown } } | null)?.message?.content)
  if (!draft) return template('unusable')
  return { kind: draft.kind, title: draft.title || untitledTitle, body: draft.body, hints: draft.hints, model: 'drafted' }
}

/** The post-scan: the rule table's categories found in a draft's text, in table order. */
export function findings(title: string, body: string): RedactionCategory[] {
  const found = new Set<RedactionCategory>()
  for (const hit of [...postScan(title), ...postScan(body)]) found.add(hit.category)
  return ORDER.filter((c) => found.has(c))
}

/**
 * "一键脱敏": the rule table, then whatever the post-scan still holds back
 * (its wider shapes, PR-Daemon #1677 non-blocking 1) replaced whole by a
 * placeholder, until the post-scan is clean. Without the second step a
 * `file:/` link or a long digit run could never be cleared by the button.
 */
export function clearFindings(text: string): string {
  let out = preRedact(text).text
  for (let pass = 0; pass < 4; pass += 1) {
    const hits = postScan(out)
    if (hits.length === 0) break
    let next = ''
    let cursor = 0
    for (const hit of hits) {
      next += out.slice(cursor, hit.start) + placeholder(hit.category)
      cursor = hit.end
    }
    out = next + out.slice(cursor)
  }
  return out
}

const ORDER: RedactionCategory[] = ['private_key', 'credential', 'email', 'phone', 'id_card', 'bank_card', 'path', 'internal_address']

/**
 * What a draft is bound to (CONTRACT-OPS-v2 W8.1): the Knowledge endpoint and
 * the set of knowledge bases chosen, never the key. `bindingForRun()` is the
 * constant 'default', which cannot tell one target from another, so drafts
 * record this instead; another endpoint or another set reads as another
 * target ("需要重新选目标库", QA N11).
 */
export function entryTarget(endpoint: string, knowledgeBaseIds: readonly string[]): string {
  const ids = [...new Set(knowledgeBaseIds)].sort()
  return `kbt1:${createHash('sha256').update(JSON.stringify([1, endpoint, ids]), 'utf8').digest('hex')}`
}
