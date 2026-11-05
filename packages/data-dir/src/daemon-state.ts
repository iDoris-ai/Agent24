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
  | { ok: false; reason: DaemonStateReason; message: string }

const fail = (reason: DaemonStateReason, message: string): DaemonStateResult => ({ ok: false, reason, message })

function isUint(v: unknown, max: number): v is number {
  return typeof v === 'number' && Number.isInteger(v) && v >= 0 && v <= max
}

/** Pure parse + validate of the file's text. Messages never contain field values (the file holds a bearer). */
export function parseDaemonState(raw: string): DaemonStateResult {
  let json: unknown
  try {
    json = JSON.parse(raw)
  } catch {
    return fail('malformed', 'not valid JSON')
  }
  if (typeof json !== 'object' || json === null || Array.isArray(json)) return fail('malformed', 'not a JSON object')
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

  const mode = o['auth_mode']
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

  // validate()
  if (state.enrollEndpoint !== undefined && authMode !== 'capabilities') {
    return fail('inconsistent', 'enroll_endpoint is valid only in capabilities auth mode')
  }
  if (authMode === 'legacy_single_token') {
    if (token === '') return fail('inconsistent', 'legacy_single_token auth mode requires a non-empty token')
  } else {
    if (token !== '') return fail('inconsistent', 'capabilities auth mode must not contain a token')
    if (generation === '') return fail('inconsistent', 'capabilities auth mode requires a daemon generation')
  }
  return { ok: true, state }
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
