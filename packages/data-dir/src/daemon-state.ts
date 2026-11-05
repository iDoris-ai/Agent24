// The single TS parser for the daemon discovery file (`<data_dir>/daemon.json`).
// Mirror of rust/crates/agent24-protocol/src/state_file.rs (`DaemonState`
// serde shape + `validate()` + `bearer_token()`); both sides are locked to
// protocol/fixtures/daemon-state/cases.json. Every TS reader (desktop, the two
// bridges, documents-accept) goes through here: none may parse daemon.json
// itself, so `auth_mode` has one meaning everywhere (ADR-038 / E3).
//
// Rules copied from Rust, field by field:
//  - port: integer 0..=65535 (u16), required.   pid: integer 0..=4294967295 (u32), required.
//  - version: string, required.
//  - token / generation: string, default "" when absent (null or non-string rejects).
//  - auth_mode: absent → legacy_single_token; "legacy_single_token" | "capabilities";
//    anything else (incl. null / non-string) rejects the whole record.
//  - protocol_version: absent/null → none, else u32. enroll_endpoint / startup_state:
//    absent/null → none, else string. A wrongly typed value rejects the record.
//  - validate(): enroll_endpoint only with capabilities; legacy needs a non-empty token;
//    capabilities must carry no token and a non-empty generation.
//  - Unknown extra fields are ignored (Rust has no deny_unknown_fields).
// pid liveness (Rust `read_live`) is deliberately not checked here: callers
// probe /health, which is the stronger signal.

import fs from 'node:fs'
import { dataDir, joinLiteral } from './index'

export type DaemonAuthModeName = 'legacy_single_token' | 'capabilities'

export interface DaemonState {
  port: number
  pid: number
  version: string
  /** '' only on legacy records written before generations existed. */
  generation: string
  authMode: DaemonAuthModeName
  /** The legacy bearer; always '' in capabilities mode. Prefer {@link daemonBearer}. */
  token: string
  protocolVersion?: number
  enrollEndpoint?: string
  startupState?: string
}

export type DaemonStateReason =
  | 'unreadable' // absent / not readable
  | 'malformed' // not JSON, or not an object
  | 'invalid' // wrong field types / unknown auth_mode
  | 'inconsistent' // fails validate(): token / generation / enroll_endpoint rules

export type DaemonStateResult =
  | { ok: true; state: DaemonState }
  | {
      ok: false
      reason: DaemonStateReason
      message: string
      /** Present when the record was refused but still names a plausible port and pid, so a caller can
       * tell "a daemon we cannot read is running" from "no daemon". Never a credential. */
      endpoint?: { port: number; pid: number }
    }

const fail = (reason: DaemonStateReason, message: string): DaemonStateResult => ({ ok: false, reason, message })

function isUint(v: unknown, max: number): v is number {
  return typeof v === 'number' && Number.isInteger(v) && v >= 0 && v <= max
}

const KNOWN_FIELDS = new Set([
  'port', 'token', 'pid', 'version', 'generation', 'auth_mode', 'protocol_version', 'enroll_endpoint', 'startup_state',
])
/** Known fields whose string value Rust decodes (and so rejects when it holds a lone surrogate). */
const STRING_FIELDS = new Set(['token', 'version', 'generation', 'auth_mode', 'enroll_endpoint', 'startup_state'])

interface TopLevelScan {
  /** First known top-level key that appears more than once, else null. */
  duplicate: string | null
  /** Raw number literal text of each top-level key whose value is a number. */
  numbers: Map<string, string>
  /** A known string field whose value holds a lone surrogate escape (serde_json refuses it; JSON.parse does not). */
  loneSurrogate: string | null
}

/** True when a raw JSON string literal (with quotes) holds an unpaired \uD800-\uDFFF escape. */
function hasLoneSurrogate(literal: string): boolean {
  for (let k = 0; k < literal.length; k++) {
    if (literal[k] !== '\\') continue
    if (literal[k + 1] !== 'u') {
      k++
      continue
    }
    const code = parseInt(literal.slice(k + 2, k + 6), 16)
    if (code >= 0xd800 && code <= 0xdbff) {
      const low = /^\\u([dD][c-fC-F][0-9a-fA-F]{2})/.exec(literal.slice(k + 6, k + 12))
      if (!low) return true
      k += 11
    } else if (code >= 0xdc00 && code <= 0xdfff) return true
    else k += 5
  }
  return false
}

/** Lexical pass over text that JSON.parse already accepted. JSON.parse hides what serde sees:
 * repeated keys (last wins), number spelling (`4321.0`, `4.321e3`, `-0` all become integers)
 * and lone surrogates. Keys are compared after JSON string decoding (`"p\\u006frt"` is `port`). */
function scanTopLevel(raw: string): TopLevelScan {
  const out: TopLevelScan = { duplicate: null, numbers: new Map(), loneSurrogate: null }
  const seen = new Set<string>()
  let depth = 0
  let expectKey = false
  let key: string | null = null
  for (let i = 0; i < raw.length; i++) {
    const ch = raw[i] as string
    if (ch === '"') {
      let j = i + 1
      while (raw[j] !== '"') j += raw[j] === '\\' ? 2 : 1
      if (depth === 1 && expectKey) {
        key = JSON.parse(raw.slice(i, j + 1)) as string
        expectKey = false
        if (KNOWN_FIELDS.has(key)) {
          if (seen.has(key) && out.duplicate === null) out.duplicate = key
          seen.add(key)
        }
      } else if (depth === 1 && key !== null) {
        if (STRING_FIELDS.has(key) && out.loneSurrogate === null && hasLoneSurrogate(raw.slice(i, j + 1))) out.loneSurrogate = key
        key = null
      }
      i = j
    } else if (ch === '{' || ch === '[') {
      depth++
      expectKey = depth === 1 && ch === '{'
      if (depth > 1) key = null
    } else if (ch === '}' || ch === ']') depth--
    else if (ch === ',' && depth === 1) {
      expectKey = true
      key = null
    } else if (depth === 1 && key !== null && (ch === '-' || (ch >= '0' && ch <= '9'))) {
      const m = /^[-+0-9.eE]+/.exec(raw.slice(i)) as RegExpExecArray
      out.numbers.set(key, m[0])
      i += m[0].length - 1
      key = null
    }
  }
  return out
}

/** Pure parse + validate of the file's text. A refused record that still names a
 * port and a pid carries them in `endpoint` (see {@link DaemonStateResult}). Messages never contain field values (the file holds a bearer). */
export function parseDaemonState(raw: string): DaemonStateResult {
  const r = parseStrict(raw)
  if (r.ok) return r
  try {
    const o = JSON.parse(raw) as Record<string, unknown> | null
    if (o && typeof o === 'object' && isUint(o['port'], 0xffff) && isUint(o['pid'], 0xffff_ffff)) {
      return { ...r, endpoint: { port: o['port'], pid: o['pid'] } }
    }
  } catch {
    /* not JSON: no endpoint */
  }
  return r
}

function parseStrict(raw: string): DaemonStateResult {
  let json: unknown
  try {
    json = JSON.parse(raw)
  } catch {
    return fail('malformed', 'not valid JSON')
  }
  if (typeof json !== 'object' || json === null || Array.isArray(json)) return fail('malformed', 'not a JSON object')
  const scan = scanTopLevel(raw)
  if (scan.duplicate !== null) return fail('invalid', `duplicate field ${scan.duplicate}`)
  // port/pid/protocol_version are unsigned integers in serde: only a plain digit run is one.
  for (const k of ['port', 'pid', 'protocol_version']) {
    const lit = scan.numbers.get(k)
    if (lit !== undefined && !/^(0|[1-9][0-9]*)$/.test(lit)) return fail('invalid', `${k} is not a plain unsigned integer`)
  }
  if (scan.loneSurrogate !== null) return fail('invalid', `${scan.loneSurrogate} holds a lone surrogate`)
  const o = json as Record<string, unknown>

  if (!isUint(o['port'], 0xffff)) return fail('invalid', 'port missing or not a u16')
  if (!isUint(o['pid'], 0xffff_ffff)) return fail('invalid', 'pid missing or not a u32')
  if (typeof o['version'] !== 'string') return fail('invalid', 'version missing or not a string')

  const strOrDefault = (key: string): string | null => {
    const v = o[key]
    if (v === undefined) return ''
    return typeof v === 'string' ? v : null
  }
  const token = strOrDefault('token')
  if (token === null) return fail('invalid', 'token is not a string')
  const generation = strOrDefault('generation')
  if (generation === null) return fail('invalid', 'generation is not a string')

  let mode = o['auth_mode']
  // serde also reads a unit variant in its map form: {"capabilities": null}.
  if (typeof mode === 'object' && mode !== null && !Array.isArray(mode)) {
    const entries = Object.entries(mode)
    if (entries.length === 1 && entries[0]?.[1] === null) mode = entries[0][0]
  }
  let authMode: DaemonAuthModeName
  if (mode === undefined || mode === 'legacy_single_token') authMode = 'legacy_single_token'
  else if (mode === 'capabilities') authMode = 'capabilities'
  else return fail('invalid', 'unknown auth_mode')

  const state: DaemonState = { port: o['port'], pid: o['pid'], version: o['version'], generation, authMode, token }

  const pv = o['protocol_version']
  if (pv !== undefined && pv !== null) {
    if (!isUint(pv, 0xffff_ffff)) return fail('invalid', 'protocol_version is not a u32')
    state.protocolVersion = pv
  }
  for (const [key, field] of [
    ['enroll_endpoint', 'enrollEndpoint'],
    ['startup_state', 'startupState'],
  ] as const) {
    const v = o[key]
    if (v === undefined || v === null) continue
    if (typeof v !== 'string') return fail('invalid', `${key} is not a string`)
    state[field] = v
  }

  const inconsistent = validateState(state)
  if (inconsistent !== null) return fail('inconsistent', inconsistent)
  return { ok: true, state }
}

/** Mirror of Rust `DaemonState::validate()`: null when consistent, else the reason. */
function validateState(state: DaemonState): string | null {
  if (state.enrollEndpoint !== undefined && state.authMode !== 'capabilities') {
    return 'enroll_endpoint is valid only in capabilities auth mode'
  }
  if (state.authMode === 'legacy_single_token') {
    if (state.token === '') return 'legacy_single_token auth mode requires a non-empty token'
  } else {
    if (state.token !== '') return 'capabilities auth mode must not contain a token'
    if (state.generation === '') return 'capabilities auth mode requires a daemon generation'
  }
  return null
}

/** `<data_dir>/daemon.json` (throws DataDirError when the root is unresolvable). */
export function daemonStateFile(): string {
  return joinLiteral(dataDir(), 'daemon.json')
}

/** Read + parse + validate. `file` defaults to `<data_dir>/daemon.json`. Never throws on file content. */
export function readDaemonState(file?: string): DaemonStateResult {
  let raw: string
  try {
    raw = fs.readFileSync(file ?? daemonStateFile(), 'utf8')
  } catch {
    return fail('unreadable', 'daemon state file absent or unreadable')
  }
  return parseDaemonState(raw)
}

export type DaemonBearer = { ok: true; token: string } | { ok: false; message: string }

/** Mirror of Rust `bearer_token()`: only a legacy record yields a bearer; capabilities fails closed. */
export function daemonBearer(state: DaemonState): DaemonBearer {
  // Like Rust: re-validate, so a hand-built or mutated state cannot yield a bearer.
  if (validateState(state) !== null) return { ok: false, message: 'invalid daemon discovery state' }
  if (state.authMode === 'capabilities') {
    return { ok: false, message: 'daemon is in capabilities auth mode: host authority is not in daemon.json' }
  }
  if (state.token === '') return { ok: false, message: 'legacy daemon discovery token missing' }
  return { ok: true, token: state.token }
}

export class DaemonStateError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'DaemonStateError'
  }
}

/** Convenience for callers that need a usable bearer or a loud failure:
 * returns port+token of a legacy record; throws DaemonStateError otherwise
 * (including a record that exists but is invalid / capabilities). `null` when
 * the file is simply absent/unreadable. */
export function requireDaemonBearer(file?: string): { port: number; token: string } | null {
  const r = readDaemonState(file)
  if (!r.ok) {
    if (r.reason === 'unreadable') return null
    throw new DaemonStateError(`daemon.json is ${r.reason}: ${r.message}`)
  }
  const b = daemonBearer(r.state)
  if (!b.ok) throw new DaemonStateError(`daemon.json: ${b.message}`)
  return { port: r.state.port, token: b.token }
}
