// KB-SOP-01b-1 (#1669): local knowledge entry drafts, one file each.
//
// Hash: CONTRACT-OPS-v2 W8.13 scheme 1, frozen in #1668. The server does not
// normalize anything; it hashes the bytes it gets. So the text is put in its
// canonical form here, when a draft is saved (NFC, every line break as LF,
// the title trimmed, no lone surrogate), and the hash is
// sha256(JSON.stringify([1, kind, title, body])) of exactly the stored text.
// A stored draft whose text is not canonical, or whose hash does not match,
// reads as missing.

import { createHash, randomBytes } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { CHAT_HISTORY_ID } from '../shared/chat-history'

export const ENTRY_KINDS = ['sop', 'faq', 'note'] as const
export type EntryKind = typeof ENTRY_KINDS[number]

export const ENTRY_DRAFT_FORMAT = 'agent24-knowledge-entry-draft'
export const ENTRY_DRAFT_VERSION = 1
export const MAX_ENTRY_DRAFTS = 50
export const MAX_ENTRY_TITLE = 200
export const MAX_ENTRY_BODY = 64 * 1024
export const ENTRY_DRAFT_ID = /^ked_[a-z0-9]{22}$/

/** `null` means 1a has not decided a floor yet. */
export type EntryFloor = 'ordinary' | 'restricted' | 'unknown' | null

export interface EntryDraft {
  format: typeof ENTRY_DRAFT_FORMAT
  version: typeof ENTRY_DRAFT_VERSION
  id: string
  binding: string
  source: { conversation_id: string; message_from: number; message_to: number }
  floor: EntryFloor
  skill_version: string
  redaction_rules_version: number
  kind: EntryKind
  title: string
  body: string
  content_sha256: string
  created_at: string
  updated_at: string
}

export type EntryDraftInput = Omit<EntryDraft, 'format' | 'version' | 'id' | 'content_sha256' | 'created_at' | 'updated_at'>

const SKILL_VERSION = /^[a-z0-9][a-z0-9._/-]{0,63}$/
const UTC_TIME = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,3})?Z$/
const DRAFT_FILE = /^ked_[a-z0-9]{22}\.json$/
const ID_ALPHABET = 'abcdefghijklmnopqrstuvwxyz0123456789'
const TMP_MAX_AGE_MS = 60 * 60 * 1000

/** W8.13 scheme 1 over the text as given: no normalization (see the top of this file). */
export function entryHash(entry: { kind: EntryKind; title: string; body: string }): string {
  return createHash('sha256').update(JSON.stringify([1, entry.kind, entry.title, entry.body]), 'utf8').digest('hex')
}

const LONE_SURROGATE = /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/

/** The canonical form W8.13 asks the client for: NFC, LF line breaks, the title trimmed. */
export function canonicalText(value: string, trim: boolean): string {
  if (LONE_SURROGATE.test(value)) fail()
  const normalized = value.normalize('NFC').replace(/\r\n?/g, '\n')
  return trim ? normalized.trim() : normalized
}

function fail(): never {
  throw new Error('Invalid entry draft')
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

function isKind(v: unknown): v is EntryKind {
  return typeof v === 'string' && (ENTRY_KINDS as readonly string[]).includes(v)
}

function isFloor(v: unknown): v is EntryFloor {
  return v === 'ordinary' || v === 'restricted' || v === 'unknown' || v === null
}

function isUtc(v: unknown): v is string {
  return typeof v === 'string' && UTC_TIME.test(v) && !Number.isNaN(Date.parse(v))
}

function isIndex(v: unknown): v is number {
  return typeof v === 'number' && Number.isInteger(v) && v >= 0 && v <= 100000
}

/** The content in canonical form (W8.13), or throws. Lengths count the canonical text. */
function checkedContent(kind: unknown, title: unknown, body: unknown): { kind: EntryKind; title: string; body: string } {
  if (!isKind(kind)) fail()
  if (typeof title !== 'string' || typeof body !== 'string') fail()
  const canonTitle = canonicalText(title, true)
  const canonBody = canonicalText(body, false)
  if (canonTitle.length < 1 || canonTitle.length > MAX_ENTRY_TITLE) fail()
  if (canonBody.length > MAX_ENTRY_BODY) fail()
  return { kind, title: canonTitle, body: canonBody }
}

/** Whether stored content is already exactly canonical. */
function isCanonical(kind: unknown, title: unknown, body: unknown): boolean {
  try {
    const content = checkedContent(kind, title, body)
    return content.title === title && content.body === body
  } catch {
    return false
  }
}

function assertInput(input: EntryDraftInput): { kind: EntryKind; title: string; body: string } {
  if (!isRecord(input)) fail()
  const content = checkedContent(input.kind, input.title, input.body)
  if (typeof input.binding !== 'string' || input.binding.length < 1 || input.binding.length > 2048) fail()
  if (!isRecord(input.source)) fail()
  if (typeof input.source.conversation_id !== 'string' || !CHAT_HISTORY_ID.test(input.source.conversation_id)) fail()
  if (!isIndex(input.source.message_from) || !isIndex(input.source.message_to)) fail()
  if (input.source.message_from > input.source.message_to) fail()
  if (!isFloor(input.floor)) fail()
  if (typeof input.skill_version !== 'string' || !SKILL_VERSION.test(input.skill_version)) fail()
  if (typeof input.redaction_rules_version !== 'number' || !Number.isInteger(input.redaction_rules_version) || input.redaction_rules_version < 1) fail()
  return content
}

function newDraftId(root: string): string {
  for (let attempt = 0; attempt < 8; attempt += 1) {
    let chars = ''
    while (chars.length < 22) {
      const buf = randomBytes(32)
      for (let i = 0; i < buf.length && chars.length < 22; i += 1) {
        const b = buf[i]
        if (b >= 252) continue
        chars += ID_ALPHABET[b % 36]
      }
    }
    const id = `ked_${chars}`
    if (!fs.existsSync(path.join(root, `${id}.json`))) return id
  }
  throw new Error('Invalid entry draft')
}

export function createEntryDraftStore(dir: string, now: () => Date = () => new Date()): {
  create(input: EntryDraftInput): EntryDraft
  update(id: unknown, patch: { kind?: unknown; title?: unknown; body?: unknown }): EntryDraft
  get(id: unknown): EntryDraft | null
  list(): EntryDraft[]
  withdraw(id: unknown): boolean
} {
  const root = path.join(dir, 'knowledge-entries')

  const ensureRoot = (): void => {
    fs.mkdirSync(root, { recursive: true, mode: 0o700 })
    fs.chmodSync(root, 0o700)
  }

  const fileOf = (id: string): string => path.join(root, `${id}.json`)

  const checkedId = (id: unknown): string | null => {
    if (typeof id !== 'string' || !ENTRY_DRAFT_ID.test(id)) return null
    return id
  }

  /** Same shape as chat-history `replace`: exclusive temp, rename, drop the temp on failure. */
  const replace = (file: string, temp: string, body: string): void => {
    ensureRoot()
    const fd = fs.openSync(temp, 'wx', 0o600)
    let renamed = false
    try {
      try { fs.writeFileSync(fd, body) }
      finally { fs.closeSync(fd) }
      fs.renameSync(temp, file)
      renamed = true
      fs.chmodSync(file, 0o600)
    } finally {
      if (!renamed) fs.rmSync(temp, { force: true })
    }
  }

  const writeDraft = (draft: EntryDraft): void => {
    const temp = path.join(root, `${draft.id}.json.${randomBytes(6).toString('hex')}.tmp`)
    replace(fileOf(draft.id), temp, JSON.stringify(draft))
  }

  const parseDraft = (value: unknown, expectedId: string): EntryDraft | null => {
    if (!isRecord(value)) return null
    if (value.format !== ENTRY_DRAFT_FORMAT || value.version !== ENTRY_DRAFT_VERSION) return null
    if (value.id !== expectedId || !checkedId(value.id)) return null
    if (typeof value.binding !== 'string' || value.binding.length < 1 || value.binding.length > 2048) return null
    if (!isRecord(value.source)) return null
    if (typeof value.source.conversation_id !== 'string' || !CHAT_HISTORY_ID.test(value.source.conversation_id)) return null
    if (!isIndex(value.source.message_from) || !isIndex(value.source.message_to)) return null
    if (value.source.message_from > value.source.message_to) return null
    if (!isFloor(value.floor)) return null
    if (typeof value.skill_version !== 'string' || !SKILL_VERSION.test(value.skill_version)) return null
    if (typeof value.redaction_rules_version !== 'number' || !Number.isInteger(value.redaction_rules_version) || value.redaction_rules_version < 1) return null
    if (!isKind(value.kind) || typeof value.title !== 'string' || typeof value.body !== 'string') return null
    if (!isCanonical(value.kind, value.title, value.body)) return null
    if (typeof value.content_sha256 !== 'string') return null
    if (value.content_sha256 !== entryHash({ kind: value.kind, title: value.title, body: value.body })) return null
    if (!isUtc(value.created_at) || !isUtc(value.updated_at)) return null
    return {
      format: ENTRY_DRAFT_FORMAT,
      version: ENTRY_DRAFT_VERSION,
      id: value.id,
      binding: value.binding,
      source: {
        conversation_id: value.source.conversation_id,
        message_from: value.source.message_from,
        message_to: value.source.message_to,
      },
      floor: value.floor,
      skill_version: value.skill_version,
      redaction_rules_version: value.redaction_rules_version,
      kind: value.kind,
      title: value.title,
      body: value.body,
      content_sha256: value.content_sha256,
      created_at: value.created_at,
      updated_at: value.updated_at,
    }
  }

  const readDraft = (id: string): EntryDraft | null => {
    try {
      const parsed = parseDraft(JSON.parse(fs.readFileSync(fileOf(id), 'utf8')), id)
      return parsed
    } catch {
      return null
    }
  }

  const names = (): string[] => {
    try { return fs.readdirSync(root) }
    catch { return [] }
  }

  const sweepTemps = (): void => {
    const cutoff = now().getTime() - TMP_MAX_AGE_MS
    for (const name of names()) {
      if (!name.endsWith('.tmp')) continue
      const file = path.join(root, name)
      try {
        const st = fs.statSync(file)
        if (st.isFile() && st.mtimeMs < cutoff) fs.rmSync(file, { force: true })
      } catch {
        // A temp we cannot stat is left for the next sweep.
      }
    }
  }

  const readAll = (): EntryDraft[] => {
    sweepTemps()
    const out: EntryDraft[] = []
    for (const name of names()) {
      if (!DRAFT_FILE.test(name)) continue
      const id = name.slice(0, -'.json'.length)
      const draft = readDraft(id)
      if (draft) out.push(draft)
    }
    return out
  }

  return {
    create(input: EntryDraftInput): EntryDraft {
      const content = assertInput(input)
      if (readAll().length >= MAX_ENTRY_DRAFTS) throw new Error('Too many entry drafts')
      const stamp = now().toISOString()
      const draft: EntryDraft = {
        format: ENTRY_DRAFT_FORMAT,
        version: ENTRY_DRAFT_VERSION,
        id: newDraftId(root),
        binding: input.binding,
        source: {
          conversation_id: input.source.conversation_id,
          message_from: input.source.message_from,
          message_to: input.source.message_to,
        },
        floor: input.floor,
        skill_version: input.skill_version,
        redaction_rules_version: input.redaction_rules_version,
        kind: content.kind,
        title: content.title,
        body: content.body,
        content_sha256: entryHash(content),
        created_at: stamp,
        updated_at: stamp,
      }
      writeDraft(draft)
      return draft
    },

    update(id: unknown, patch: { kind?: unknown; title?: unknown; body?: unknown }): EntryDraft {
      const checked = checkedId(id)
      if (!checked) fail()
      const current = readDraft(checked)
      if (!current) throw new Error('Entry draft not found')
      const content = checkedContent(
        patch.kind === undefined ? current.kind : patch.kind,
        patch.title === undefined ? current.title : patch.title,
        patch.body === undefined ? current.body : patch.body,
      )
      const next: EntryDraft = {
        ...current,
        kind: content.kind,
        title: content.title,
        body: content.body,
        content_sha256: entryHash(content),
        updated_at: now().toISOString(),
      }
      writeDraft(next)
      return next
    },

    get(id: unknown): EntryDraft | null {
      const checked = checkedId(id)
      if (!checked) return null
      return readDraft(checked)
    },

    list(): EntryDraft[] {
      return readAll().sort((a, b) => (a.updated_at < b.updated_at ? 1 : a.updated_at > b.updated_at ? -1 : 0))
    },

    withdraw(id: unknown): boolean {
      const checked = checkedId(id)
      if (!checked) return false
      const current = readDraft(checked)
      if (!current) return false
      // The draft leaves the store first, by a rename (one step, undone if
      // the event cannot be written), so a withdrawal is either recorded and
      // gone, or neither. Removing the renamed file may still fail; it is a
      // temp then, outside the store, and the temp sweep takes it later.
      ensureRoot()
      const aside = path.join(root, `${checked}.withdrawn.tmp`)
      fs.renameSync(fileOf(checked), aside)
      try {
        const events = path.join(root, '_events.jsonl')
        const line = `${JSON.stringify({
          at: now().toISOString(),
          id: current.id,
          event: 'withdrawn',
          content_sha256: current.content_sha256,
        })}\n`
        const fd = fs.openSync(events, 'a', 0o600)
        try { fs.writeFileSync(fd, line) }
        finally { fs.closeSync(fd) }
        fs.chmodSync(events, 0o600)
      } catch (err) {
        fs.renameSync(aside, fileOf(checked))
        throw err
      }
      try { fs.rmSync(aside, { force: true }) }
      catch { /* swept later as a stale temp */ }
      return true
    },
  }
}
