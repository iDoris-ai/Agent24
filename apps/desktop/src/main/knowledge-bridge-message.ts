// KB-C2-04a-2 (#1623): reading one page→host bridge message the way
// CONTRACT-UI-BRIDGE-v1 says, steps ⑦–⑫ of U6.4 — length, strict JSON,
// forbidden keys, version, envelope and payload schema (U7.1–U7.4, U6.7). No
// state here: knowledge-bridge-judge.ts runs the steps around these (①–⑥,
// ⑬–⑯) and keeps the per-document tables.

/** U6.7: one page message is at most 8 KiB on the wire. */
export const BRIDGE_MAX_MESSAGE_BYTES = 8 * 1024
/** U6.7: nesting, the root object being level 1. */
export const BRIDGE_MAX_DEPTH = 4
export const BRIDGE_MAX_ARRAY = 32
/** U6.7: a string (key or value), counted in UTF-8 bytes. */
export const BRIDGE_MAX_STRING_BYTES = 2 * 1024
/** U7.1: the one protocol version (3 since decision #37 added `document_id`). */
export const BRIDGE_VERSION = 3
export const BRIDGE_SUPPORTED: readonly number[] = [BRIDGE_VERSION]

export const ULID = /^[0-9A-HJKMNP-TV-Z]{26}$/
/** U7.1 (decision #37): the host's per-document value, random, not a secret. */
export const DOCUMENT_ID = /^[A-Za-z0-9_-]{22}$/
const MAX_GENERATION = Number.MAX_SAFE_INTEGER

/** U7.4: compared after lower-casing and dropping `_` and `-`; never by substring. */
const FORBIDDEN_KEYS = new Set([
  'url', 'uri', 'href', 'path', 'file', 'dir', 'cwd', 'command', 'cmd', 'exec', 'executable', 'argv', 'args', 'shell',
  'script', 'approval', 'approve', 'decision', 'token', 'jwt', 'ticket', 'secret', 'password', 'authorization', 'cookie',
  'origin', 'sender', 'frame', 'webcontents', 'port', '__proto__', 'constructor', 'prototype',
].map((k) => k.replace(/[_-]/g, '')))

export function isForbiddenKey(key: string): boolean {
  return FORBIDDEN_KEYS.has(key.toLowerCase().replace(/[_-]/g, ''))
}

// ── ⑧ strict JSON ────────────────────────────────────────────────────────────

export type Json = null | boolean | number | string | Json[] | JsonObject
/** Built without a prototype, so `__proto__` and friends are plain keys (U7.4). */
export type JsonObject = { [key: string]: Json }

const utf8 = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true })
const byteLength = (s: string): number => Buffer.byteLength(s, 'utf8')

class Reject extends Error {}

/**
 * ⑧: valid UTF-8 JSON, no duplicate key at any level, nesting, arrays and
 * strings within U6.7. Null for anything else. Stricter than JSON.parse: a
 * leading BOM, a lone surrogate escape and trailing data are refused.
 */
export function parseBridgeJson(bytes: Uint8Array): Json | undefined {
  let text: string
  try {
    text = utf8.decode(bytes)
  } catch {
    return undefined
  }
  let i = 0
  const ws = (): void => { while (i < text.length && (text[i] === ' ' || text[i] === '\t' || text[i] === '\n' || text[i] === '\r')) i++ }
  const fail = (): never => { throw new Reject() }

  const str = (): string => {
    if (text[i] !== '"') fail()
    i++
    let out = ''
    for (;;) {
      if (i >= text.length) fail()
      const c = text.charCodeAt(i)
      if (c === 0x22) { i++; break }
      if (c < 0x20) fail()
      if (c !== 0x5c) { out += text[i++]; continue }
      const e = text[i + 1]
      i += 2
      if (e === '"' || e === '\\' || e === '/') out += e
      else if (e === 'b') out += '\b'
      else if (e === 'f') out += '\f'
      else if (e === 'n') out += '\n'
      else if (e === 'r') out += '\r'
      else if (e === 't') out += '\t'
      else if (e === 'u') {
        const hex = text.slice(i, i + 4)
        if (!/^[0-9a-fA-F]{4}$/.test(hex)) fail()
        out += String.fromCharCode(parseInt(hex, 16))
        i += 4
      } else fail()
    }
    // A lone surrogate is not text (U7.1's IDs are UTF-8).
    if (/[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/.test(out)) fail()
    if (byteLength(out) > BRIDGE_MAX_STRING_BYTES) fail()
    return out
  }

  const num = (): number => {
    const m = /^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/.exec(text.slice(i))
    if (!m) fail()
    i += m![0].length
    const n = Number(m![0])
    if (!Number.isFinite(n)) fail()
    return n
  }

  const value = (depth: number): Json => {
    ws()
    const c = text[i]
    if (c === '{') {
      if (depth > BRIDGE_MAX_DEPTH) fail()
      i++
      const obj = Object.create(null) as JsonObject
      ws()
      if (text[i] === '}') { i++; return obj }
      for (;;) {
        ws()
        const key = str()
        if (Object.prototype.hasOwnProperty.call(obj, key)) fail()
        ws()
        if (text[i] !== ':') fail()
        i++
        Object.defineProperty(obj, key, { value: value(depth + 1), enumerable: true, writable: true, configurable: true })
        ws()
        if (text[i] === ',') { i++; continue }
        if (text[i] === '}') { i++; return obj }
        fail()
      }
    }
    if (c === '[') {
      if (depth > BRIDGE_MAX_DEPTH) fail()
      i++
      const arr: Json[] = []
      ws()
      if (text[i] === ']') { i++; return arr }
      for (;;) {
        if (arr.length >= BRIDGE_MAX_ARRAY) fail()
        arr.push(value(depth + 1))
        ws()
        if (text[i] === ',') { i++; continue }
        if (text[i] === ']') { i++; return arr }
        fail()
      }
    }
    if (c === '"') return str()
    if (text.startsWith('true', i)) { i += 4; return true }
    if (text.startsWith('false', i)) { i += 5; return false }
    if (text.startsWith('null', i)) { i += 4; return null }
    return num()
  }

  try {
    const out = value(1)
    ws()
    return i === text.length ? out : undefined
  } catch (error) {
    if (error instanceof Reject) return undefined
    throw error
  }
}

/** ⑨: a forbidden key anywhere (U7.4). */
export function hasForbiddenKey(value: Json): boolean {
  if (Array.isArray(value)) return value.some(hasForbiddenKey)
  if (value === null || typeof value !== 'object') return false
  return Object.keys(value).some((k) => isForbiddenKey(k) || hasForbiddenKey(value[k]))
}

// ── ⑪⑫ envelope and schema ───────────────────────────────────────────────────

export const PAGE_TYPES = ['status', 'citation', 'cancel', 'return_to_chat_intent'] as const
export type PageType = (typeof PAGE_TYPES)[number]
const HOST_TYPES = ['hello', 'state', 'reply']

export interface CitationRef {
  knowledge_base_id: string
  knowledge_id: string
  chunk_id: string
  source_revision: string
  index_build_id: string
  chunk_revision: number
  content_tag: string
}

export type PagePayload =
  | { type: 'status'; state: 'loading' | 'ready' | 'error'; error_code?: 'unauthorized' | 'forbidden' | 'not_found' | 'unavailable' | 'internal' }
  | { type: 'citation'; citation: CitationRef }
  | { type: 'cancel'; target_id: string }
  | { type: 'return_to_chat_intent'; citations: CitationRef[] }

export interface PageMessage {
  id: string
  generation: number
  payload: PagePayload
  /** U6.3: a `status` with generation 0 asks for `hello`. */
  helloRequest: boolean
}

export type SchemaRead =
  | { ok: true; message: PageMessage }
  /** `counted: false` only when the one thing wrong is U7.1's default ID refusal (decision #21). */
  | { ok: false; counted: boolean }

const isObject = (v: Json | undefined): v is JsonObject => typeof v === 'object' && v !== null && !Array.isArray(v)
const onlyKeys = (o: JsonObject, required: readonly string[], optional: readonly string[] = []): boolean =>
  required.every((k) => k in o) && Object.keys(o).every((k) => required.includes(k) || optional.includes(k))

/** U7.1 reference IDs: 'bad' is a schema violation; 'refused' is the default refusal, not counted. */
function refId(v: Json | undefined): 'ok' | 'refused' | 'bad' {
  if (typeof v !== 'string') return 'bad'
  const n = byteLength(v)
  // eslint-disable-next-line no-control-regex
  if (n < 1 || n > 128 || /[\u0000-\u001f\u007f-\u009f]/.test(v)) return 'bad'
  if (/[/\\:\s]|\.\./.test(v)) return 'refused'
  return 'ok'
}

const REF_FIELDS = ['knowledge_base_id', 'knowledge_id', 'chunk_id', 'source_revision', 'index_build_id', 'content_tag'] as const

function citationRef(v: Json | undefined): CitationRef | 'refused' | null {
  if (!isObject(v) || !onlyKeys(v, [...REF_FIELDS, 'chunk_revision'])) return null
  let refused = false
  for (const k of REF_FIELDS) {
    const r = refId(v[k])
    if (r === 'bad') return null
    if (r === 'refused') refused = true
  }
  const rev = v.chunk_revision
  if (typeof rev !== 'number' || !Number.isInteger(rev) || rev < 0 || rev > 2 ** 31 - 1) return null
  return refused ? 'refused' : (v as unknown as CitationRef)
}

const STATES = ['loading', 'ready', 'error']
const ERROR_CODES = ['unauthorized', 'forbidden', 'not_found', 'unavailable', 'internal']

function payloadOf(type: PageType, p: Json | undefined): PagePayload | 'refused' | null {
  if (!isObject(p)) return null
  if (type === 'status') {
    if (!onlyKeys(p, ['state'], ['error_code']) || typeof p.state !== 'string' || !STATES.includes(p.state)) return null
    if (p.state === 'error') {
      if ('error_code' in p && (typeof p.error_code !== 'string' || !ERROR_CODES.includes(p.error_code))) return null
    } else if ('error_code' in p) return null
    return { type, ...(p as object) } as PagePayload
  }
  if (type === 'citation') {
    const ref = citationRef(p)
    if (ref === null || ref === 'refused') return ref
    return { type, citation: ref }
  }
  if (type === 'cancel') {
    if (!onlyKeys(p, ['target_id']) || typeof p.target_id !== 'string' || !ULID.test(p.target_id)) return null
    return { type, target_id: p.target_id }
  }
  if (!onlyKeys(p, ['citations']) || !Array.isArray(p.citations) || p.citations.length > 8) return null
  const refs: CitationRef[] = []
  let refused = false
  for (const c of p.citations) {
    const ref = citationRef(c)
    if (ref === null) return null
    if (ref === 'refused') refused = true
    else refs.push(ref)
  }
  return refused ? 'refused' : { type, citations: refs }
}

/** The envelope's `id` when it is a valid ULID: what a reply's `in_reply_to` carries (U7.1). */
export function envelopeId(value: Json): string | undefined {
  return isObject(value) && typeof value.id === 'string' && ULID.test(value.id) ? value.id : undefined
}

/**
 * ⑤ for what only the body can say (decision #37): the envelope's
 * `document_id`, or undefined when there is none (a hello request may come
 * without; ⑫ refuses any other message that lacks it).
 */
export function envelopeDocumentId(value: Json): Json | undefined {
  return isObject(value) && 'document_id' in value ? value.document_id : undefined
}

/** ⑪: 'missing' falls through to ⑫ (U6.4). */
export function versionOf(value: Json): 'supported' | 'unsupported' | 'missing' {
  if (!isObject(value) || !('v' in value)) return 'missing'
  return value.v === BRIDGE_VERSION ? 'supported' : 'unsupported'
}

/** ⑫: the U7.1 envelope with a page→host type and its payload. */
export function readPageMessage(value: Json): SchemaRead {
  if (!isObject(value) || !onlyKeys(value, ['v', 'direction', 'type', 'id', 'generation', 'payload'], ['document_id'])) return { ok: false, counted: true }
  const { v, direction, type, id, generation } = value
  if (v !== BRIDGE_VERSION || direction !== 'page_to_host' || typeof type !== 'string') return { ok: false, counted: true }
  if (!(PAGE_TYPES as readonly string[]).includes(type) || HOST_TYPES.includes(type)) return { ok: false, counted: true }
  if (typeof id !== 'string' || !ULID.test(id)) return { ok: false, counted: true }
  if (typeof generation !== 'number' || !Number.isInteger(generation) || generation < 0 || generation > MAX_GENERATION) return { ok: false, counted: true }
  const helloRequest = generation === 0
  if (helloRequest && type !== 'status') return { ok: false, counted: true }
  // U7.1: required on every page message but a hello request.
  if ('document_id' in value ? typeof value.document_id !== 'string' || !DOCUMENT_ID.test(value.document_id) : !helloRequest) return { ok: false, counted: true }
  const payload = payloadOf(type as PageType, value.payload)
  if (payload === null) return { ok: false, counted: true }
  if (payload === 'refused') return { ok: false, counted: false }
  return { ok: true, message: { id, generation, payload, helloRequest } }
}

// ── host→page ────────────────────────────────────────────────────────────────

/** U6.7: a host message is at most 32 KiB. */
export const BRIDGE_MAX_HOST_MESSAGE_BYTES = 32 * 1024

export type ReplyBody = { result: 'accepted' | 'rejected' | 'done'; code?: string; reason?: string; retry_after_ms?: number; supported?: number[] }

export type HostMessage =
  | { type: 'hello'; payload: { supported: number[]; view_instance_id: string } }
  | { type: 'state'; payload: { profile_state: string; code?: string; theme: 'light' | 'dark'; locale: string } }
  | { type: 'reply'; in_reply_to?: string; payload: ReplyBody }

/**
 * The wire form of a host message (U7.1, U7.3), or null when it may not be
 * sent: a forbidden key anywhere or over 32 KiB (U7.3: checked before sending).
 */
export function encodeHostMessage(message: HostMessage, id: string, generation: number, documentId: string): string | null {
  const envelope: Record<string, unknown> = { v: BRIDGE_VERSION, direction: 'host_to_page', type: message.type, id, generation, document_id: documentId }
  if (message.type === 'reply' && message.in_reply_to !== undefined) envelope.in_reply_to = message.in_reply_to
  envelope.payload = message.payload
  if (hasForbiddenKeyDeep(envelope)) return null
  const text = JSON.stringify(envelope)
  return byteLength(text) <= BRIDGE_MAX_HOST_MESSAGE_BYTES ? text : null
}

function hasForbiddenKeyDeep(value: unknown): boolean {
  if (Array.isArray(value)) return value.some(hasForbiddenKeyDeep)
  if (value === null || typeof value !== 'object') return false
  return Object.entries(value).some(([k, v]) => isForbiddenKey(k) || hasForbiddenKeyDeep(v))
}
