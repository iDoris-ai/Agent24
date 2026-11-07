import { randomBytes } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import {
  CHAT_HISTORY_FORMAT,
  CHAT_HISTORY_ID,
  CHAT_HISTORY_VERSION,
  type ChatHistoryLatency,
  type ChatHistoryMessage,
  type ChatHistoryRecord,
  type ChatHistorySummary,
} from '../shared/chat-history'

/** One conversation's file may not exceed this, so a runaway page cannot
 *  fill the disk through one IPC call. */
const MAX_RECORD_BYTES = 8 * 1024 * 1024
/** Fewer bytes than the fixed part of any written message, its strings
 *  left empty: `{"role":"user","content":"","at":"","delivered":true}` is
 *  52; the strings are counted separately below. */
const MIN_MESSAGE_BYTES = 48

/** A cheap lower bound of the written size, checked before the full parse,
 *  so an oversized save is refused without first parsing and serializing
 *  all of it on the main thread. Never more than the real size (a UTF-16
 *  unit is at least one UTF-8 byte), so a record that fits is never refused. */
function precheck(v: unknown): void {
  if (!isObject(v)) return
  const strings = (...xs: unknown[]) => xs.reduce<number>((n, x) => n + (typeof x === 'string' ? x.length : 0), 0)
  let bytes = strings(v.id, v.title, v.created_at, v.updated_at)
  const tooLarge = () => { if (bytes > MAX_RECORD_BYTES) throw new Error('Chat history record too large') }
  tooLarge()
  if (!Array.isArray(v.messages)) return
  for (const m of v.messages) {
    bytes += MIN_MESSAGE_BYTES
    if (isObject(m)) {
      bytes += strings(m.content, m.at, m.run_id)
      if (isObject(m.latency)) bytes += strings(m.latency.modelName, m.latency.tier)
    }
    tooLarge()
  }
}

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v)
/** ISO 8601 in UTC, as `Date.prototype.toISOString` writes it: no local
 *  times, so the order and the instant survive a move to another zone. */
const UTC_TIME = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,3})?Z$/
const isTime = (v: unknown): v is string => typeof v === 'string' && UTC_TIME.test(v) && !Number.isNaN(Date.parse(v))
const isNullableString = (v: unknown): v is string | null => v === null || typeof v === 'string'
const isNullableNumber = (v: unknown): v is number | null => v === null || (typeof v === 'number' && Number.isFinite(v))

function invalid(): never {
  throw new Error('Invalid chat history record')
}

function parseLatency(v: unknown): ChatHistoryLatency | null {
  if (v === null) return null
  if (!isObject(v) || !isNullableString(v.modelName) || !isNullableString(v.tier)
    || !isNullableNumber(v.firstTokenMs) || typeof v.totalMs !== 'number' || !Number.isFinite(v.totalMs)) invalid()
  return { modelName: v.modelName, tier: v.tier, firstTokenMs: v.firstTokenMs, totalMs: v.totalMs }
}

function parseMessage(v: unknown): ChatHistoryMessage {
  if (!isObject(v) || typeof v.content !== 'string' || !isTime(v.at)) invalid()
  if (v.role === 'user') {
    if (typeof v.delivered !== 'boolean') invalid()
    return { role: 'user', content: v.content, at: v.at, delivered: v.delivered }
  }
  if (v.role === 'assistant') {
    if (!isNullableString(v.run_id)) invalid()
    return { role: 'assistant', content: v.content, at: v.at, run_id: v.run_id, latency: parseLatency(v.latency) }
  }
  invalid()
}

/** Strict: unknown versions and malformed fields are refused, never guessed at. */
export function parseChatHistoryRecord(v: unknown): ChatHistoryRecord {
  if (!isObject(v) || v.format !== CHAT_HISTORY_FORMAT || v.version !== CHAT_HISTORY_VERSION
    || typeof v.id !== 'string' || !CHAT_HISTORY_ID.test(v.id) || typeof v.title !== 'string'
    || !isTime(v.created_at) || !isTime(v.updated_at) || !Array.isArray(v.messages)) invalid()
  return {
    format: CHAT_HISTORY_FORMAT,
    version: CHAT_HISTORY_VERSION,
    id: v.id,
    title: v.title,
    created_at: v.created_at,
    updated_at: v.updated_at,
    // Array.from visits holes as undefined, which parseMessage refuses: a
    // sparse array would otherwise be written with nulls and spoil the file.
    messages: Array.from(v.messages as unknown[], parseMessage),
  }
}

/** The main chat's history: one JSON file per conversation under
 *  `<dir>/chat-history/`, written by atomic replacement like nav-prefs.
 *  A damaged or foreign file is skipped by `list` and reads as missing. */
export function createChatHistoryStore(dir: string) {
  const root = path.join(dir, 'chat-history')
  const checkId = (id: unknown): string => {
    if (typeof id !== 'string' || !CHAT_HISTORY_ID.test(id)) throw new Error('Invalid chat history id')
    return id
  }
  const fileOf = (id: unknown): string => path.join(root, `${checkId(id)}.json`)
  /** A save's temporary file: `<id>.json.<random>.tmp`. One left by a save
   *  that died before its rename still holds the conversation, so delete and
   *  clear remove these too. */
  const tempOf = (id: string): string => path.join(root, `${id}.json.${randomBytes(6).toString('hex')}.tmp`)
  const tempOwner = (name: string): string | null => {
    const m = /^(chat_[a-z0-9_]{1,80})\.json\.[0-9a-f]{12}\.tmp$/.exec(name)
    return m ? m[1] : null
  }
  const names = (): string[] => {
    try { return fs.readdirSync(root) }
    catch { return [] }
  }
  const read = (file: string): ChatHistoryRecord | null => {
    try { return parseChatHistoryRecord(JSON.parse(fs.readFileSync(file, 'utf8'))) }
    catch { return null }
  }
  return {
    /** Newest first. */
    list(): ChatHistorySummary[] {
      return names()
        .filter(name => name.endsWith('.json') && CHAT_HISTORY_ID.test(name.slice(0, -5)))
        .flatMap(name => {
          const record = read(path.join(root, name))
          return record && `${record.id}.json` === name
            ? [{ id: record.id, title: record.title, created_at: record.created_at, updated_at: record.updated_at }]
            : []
        })
        .sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at))
    },
    get(id: unknown): ChatHistoryRecord | null {
      const record = read(fileOf(id))
      return record && record.id === id ? record : null
    },
    save(value: unknown): void {
      precheck(value)
      const record = parseChatHistoryRecord(value)
      const body = JSON.stringify(record)
      if (Buffer.byteLength(body) > MAX_RECORD_BYTES) throw new Error('Chat history record too large')
      const file = fileOf(record.id)
      fs.mkdirSync(root, { recursive: true, mode: 0o700 })
      // A fresh name, created exclusively: never follows or reuses a file
      // already there, and is ours to remove.
      const temp = tempOf(record.id)
      const fd = fs.openSync(temp, 'wx', 0o600)
      let renamed = false
      try {
        try { fs.writeFileSync(fd, body) }
        finally { fs.closeSync(fd) }
        fs.renameSync(temp, file)
        renamed = true
      } finally {
        // Ours from the moment it was opened: a failed or partial write
        // (e.g. a full disk) leaves no piece of the transcript behind.
        if (!renamed) fs.rmSync(temp, { force: true })
      }
    },
    delete(id: unknown): void {
      const own = checkId(id)
      fs.rmSync(fileOf(own), { force: true })
      for (const name of names()) if (tempOwner(name) === own) fs.rmSync(path.join(root, name), { force: true })
    },
    /** Removes every conversation this store wrote, including temporary files
     *  of interrupted saves; leaves anything else alone. */
    clear(): void {
      for (const name of names()) {
        const conversation = name.endsWith('.json') && CHAT_HISTORY_ID.test(name.slice(0, -5))
        if (conversation || tempOwner(name) !== null) fs.rmSync(path.join(root, name), { force: true })
      }
    },
  }
}
