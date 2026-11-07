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

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v)
const isTime = (v: unknown): v is string => typeof v === 'string' && v.length <= 40 && !Number.isNaN(Date.parse(v))
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
    messages: v.messages.map(parseMessage),
  }
}

/** The main chat's history: one JSON file per conversation under
 *  `<dir>/chat-history/`, written by atomic replacement like nav-prefs.
 *  A damaged or foreign file is skipped by `list` and reads as missing. */
export function createChatHistoryStore(dir: string) {
  const root = path.join(dir, 'chat-history')
  const fileOf = (id: unknown): string => {
    if (typeof id !== 'string' || !CHAT_HISTORY_ID.test(id)) throw new Error('Invalid chat history id')
    return path.join(root, `${id}.json`)
  }
  const read = (file: string): ChatHistoryRecord | null => {
    try { return parseChatHistoryRecord(JSON.parse(fs.readFileSync(file, 'utf8'))) }
    catch { return null }
  }
  return {
    /** Newest first. */
    list(): ChatHistorySummary[] {
      let names: string[]
      try { names = fs.readdirSync(root) }
      catch { return [] }
      return names
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
      const record = parseChatHistoryRecord(value)
      const body = JSON.stringify(record)
      if (Buffer.byteLength(body) > MAX_RECORD_BYTES) throw new Error('Chat history record too large')
      const file = fileOf(record.id)
      const temp = `${file}.tmp`
      try {
        fs.mkdirSync(root, { recursive: true, mode: 0o700 })
        fs.writeFileSync(temp, body, { mode: 0o600 })
        fs.renameSync(temp, file)
      } finally {
        fs.rmSync(temp, { force: true })
      }
    },
    delete(id: unknown): void {
      fs.rmSync(fileOf(id), { force: true })
    },
    /** Removes every conversation this store wrote; leaves anything else alone. */
    clear(): void {
      let names: string[]
      try { names = fs.readdirSync(root) }
      catch { return }
      for (const name of names) {
        if (name.endsWith('.json') && CHAT_HISTORY_ID.test(name.slice(0, -5))) fs.rmSync(path.join(root, name), { force: true })
      }
    },
  }
}
