// F3 WeChat bridge configuration.
//
// Two connections: OUT to WeChat via the official iLink Bot API, and IN to the
// local agent24d over its v1 HTTP API. The iLink constants are ground-truth
// values (ported from heinu1 / weixin-bot-ilink); the agent24d endpoint is
// discovered from the daemon's state file, overridable by env.
//
// The bridge does not keep an allowlist. Who may start a run is the daemon's
// `inbound_allowlist`. The bearer is the dev-bridge file under `dataDir()`,
// not a field of `daemon.json`.

import { createHash, randomUUID } from 'node:crypto'
import fs from 'node:fs'

import { DaemonStateError, dataDir, joinLiteral, readDaemonState } from '@agent24/data-dir'

export const CONFIG = {
  // ── WeChat iLink (official bot API) ──────────────────────────────────────
  // Domain only — the `/ilink/bot/*` prefix is in each endpoint path.
  ILINK_BASE: process.env.A24_ILINK_BASE || 'https://ilinkai.weixin.qq.com',
  // Where the bot_token from the QR login is persisted, so you scan only once.
  // Read on each access so AGENT24_DATA_DIR / HOME are the process's current values.
  get TOKEN_FILE(): string {
    return joinLiteral(dataDir(), 'wechat-token.json')
  },
  POLL_TIMEOUT_MS: 40_000,
  RECONNECT_DELAY_MS: 3_000,
  /** Ceiling for the hold backoff. Tests lower it; production stays at 60s. */
  HOLD_BACKOFF_CAP_MS: 60_000,
  MAX_MSG_LEN: 1800,

  // ── agent24d (the local daemon this bridge posts events to) ──────────────
  // The daemon writes its dynamic port + token to this discovery file (Rust
  // side). Env overrides win for tests / non-standard setups.
  get DAEMON_STATE_FILE(): string {
    return joinLiteral(dataDir(), 'daemon.json')
  },
  // Development ingest. The route fixes source to wechat.
  INBOUND_EVENTS_PATH: '/api/v1/inbound/events',
  // Dev-bridge credential. Same data root as the daemon (`dataDir()`), under `dev/`.
  get BRIDGE_TOKEN_FILE(): string {
    return joinLiteral(dataDir(), 'dev', 'wechat-bridge.token')
  },
  /** Last committed iLink `get_updates_buf`, beside the token file. */
  get BRIDGE_CURSOR_FILE(): string {
    return joinLiteral(dataDir(), 'dev', 'wechat-bridge.cursor')
  },
}

export const BRIDGE_TOKEN_HINT = '请运行 `agent24 dev bridge-token`'

// iLink `base_info.channel_version`, from the reference implementation.
export const BASE_INFO = { channel_version: '1.0.0' }

export interface DaemonEndpoint {
  base: string
  token: string
}

export interface BridgeCredential {
  token: string
  expiresAt: number
}

// O_NONBLOCK: a planted FIFO must not stall this synchronous open. A real
// file ignores the flag; fstat below still requires a regular file.
const OPEN_NOFOLLOW = fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK

function assertPrivateDir(dir: string): void {
  let st: fs.Stats
  try {
    st = fs.lstatSync(dir)
  } catch {
    throw new Error(`无法读取微信桥目录 ${dir}。${BRIDGE_TOKEN_HINT}`)
  }
  if (st.isSymbolicLink() || !st.isDirectory() || (st.mode & 0o777) !== 0o700) {
    throw new Error(`微信桥目录必须是模式 0700 的真实目录（${dir}）。${BRIDGE_TOKEN_HINT}`)
  }
}

/** Open `path` without following a symlink, then check the fd. */
function readPrivateFile(path: string, mode: number): string {
  let fd: number
  try {
    fd = fs.openSync(path, OPEN_NOFOLLOW)
  } catch (err) {
    const code = (err as NodeJS.ErrnoException).code
    if (code === 'ENOENT') throw new Error(`没有微信桥令牌文件 ${path}。${BRIDGE_TOKEN_HINT}`)
    if (code === 'ELOOP') throw new Error(`微信桥令牌文件不能是符号链接（${path}）。${BRIDGE_TOKEN_HINT}`)
    throw new Error(`无法读取微信桥令牌文件 ${path}。${BRIDGE_TOKEN_HINT}`)
  }
  try {
    const st = fs.fstatSync(fd)
    if (!st.isFile() || (st.mode & 0o777) !== mode) {
      throw new Error(`微信桥令牌文件权限必须是 0${mode.toString(8)}（${path}）。${BRIDGE_TOKEN_HINT}`)
    }
    return fs.readFileSync(fd, 'utf8')
  } finally {
    fs.closeSync(fd)
  }
}

/** Read `dev/wechat-bridge.token`. Missing, not mode 0600, a symlink, or expired: refuse.
 * The parent directory must be a real 0700 directory. This does not open `daemon.json`. */
export function loadBridgeCredential(nowMs = Date.now()): BridgeCredential {
  const path = CONFIG.BRIDGE_TOKEN_FILE
  assertPrivateDir(path.slice(0, path.lastIndexOf('/')))
  let parsed: { token?: unknown; expires_at?: unknown }
  try {
    parsed = JSON.parse(readPrivateFile(path, 0o600)) as { token?: unknown; expires_at?: unknown }
  } catch (err) {
    if (err instanceof Error && err.message.includes('微信桥')) throw err
    throw new Error(`微信桥令牌文件无法解析（${path}）。${BRIDGE_TOKEN_HINT}`)
  }
  if (typeof parsed.token !== 'string' || parsed.token.length === 0) {
    throw new Error(`微信桥令牌文件没有令牌（${path}）。${BRIDGE_TOKEN_HINT}`)
  }
  if (typeof parsed.expires_at !== 'number' || !Number.isInteger(parsed.expires_at)) {
    throw new Error(`微信桥令牌文件没有过期时间（${path}）。${BRIDGE_TOKEN_HINT}`)
  }
  if (parsed.expires_at * 1000 <= nowMs) {
    throw new Error(`微信桥令牌已过期（${path}）。${BRIDGE_TOKEN_HINT}`)
  }
  return { token: parsed.token, expiresAt: parsed.expires_at }
}

/** Stable id for one iLink login. The saved token has no separate bot id;
 * this is the sha256 of `bot_token`, so the cursor file does not store the bearer. */
export function ilinkAccountKey(botToken: string): string {
  return createHash('sha256').update(botToken).digest('hex')
}

/** Last committed cursor for `account`. Missing, unreadable, a symlink, or a
 * different iLink account: start from empty. A mismatch is logged. */
export function loadBridgeCursor(account = ''): string {
  const path = CONFIG.BRIDGE_CURSOR_FILE
  let fd: number
  try {
    fd = fs.openSync(path, OPEN_NOFOLLOW)
  } catch {
    return ''
  }
  try {
    const st = fs.fstatSync(fd)
    if (!st.isFile() || (st.mode & 0o777) !== 0o600) return ''
    return cursorForAccount(fs.readFileSync(fd, 'utf8'), account)
  } catch {
    return ''
  } finally {
    fs.closeSync(fd)
  }
}

function cursorForAccount(text: string, account: string): string {
  let parsed: { account?: unknown; cursor?: unknown }
  try {
    parsed = JSON.parse(text) as { account?: unknown; cursor?: unknown }
  } catch {
    console.error('[wechat] 游标账号与当前 iLink 登录不一致，忽略旧游标')
    return ''
  }
  if (parsed.account !== account || typeof parsed.cursor !== 'string') {
    console.error('[wechat] 游标账号与当前 iLink 登录不一致，忽略旧游标')
    return ''
  }
  return parsed.cursor
}

/** Atomically publish the cursor next to the token (0600, temp file + rename).
 * The file records `account` so a later login does not resume another bot's cursor. */
export function saveBridgeCursor(cursor: string, account = ''): void {
  const path = CONFIG.BRIDGE_CURSOR_FILE
  const dir = path.slice(0, path.lastIndexOf('/'))
  assertPrivateDir(dir)
  const tmp = `${path}.${randomUUID()}.tmp`
  const body = JSON.stringify({ account, cursor })
  let fd: number
  try {
    fd = fs.openSync(tmp, fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_NOFOLLOW, 0o600)
  } catch (err) {
    throw new Error(`无法写入微信桥游标 ${path}: ${err instanceof Error ? err.message : err}`)
  }
  try {
    fs.writeFileSync(fd, body, 'utf8')
    fs.fchmodSync(fd, 0o600)
    fs.fsyncSync(fd)
  } finally {
    fs.closeSync(fd)
  }
  try {
    fs.renameSync(tmp, path)
  } catch (err) {
    fs.rmSync(tmp, { force: true })
    throw err
  }
  fsyncDir(dir)
}

/** fsync the directory so the rename is durable. A failure is logged; the
 * rename already published the file. */
function fsyncDir(dir: string): void {
  let fd: number | undefined
  try {
    fd = fs.openSync(dir, 'r')
    fs.fsyncSync(fd)
  } catch (err) {
    console.error('[wechat] 游标目录 fsync 失败:', err instanceof Error ? err.message : err)
  } finally {
    if (fd !== undefined) {
      try {
        fs.closeSync(fd)
      } catch {
        // The fsync error, if any, was already logged.
      }
    }
  }
}

const LOOPBACK_HOSTS = new Set(['127.0.0.1', 'localhost', '::1', '[::1]'])

function daemonBase(): string {
  const envBase = process.env.A24_BASE_URL
  if (envBase) {
    let url: URL
    try {
      url = new URL(envBase)
    } catch {
      throw new DaemonStateError('A24_BASE_URL is not a valid URL')
    }
    if (url.protocol !== 'http:') throw new DaemonStateError('A24_BASE_URL must be http')
    if (url.username !== '' || url.password !== '') {
      throw new DaemonStateError('A24_BASE_URL 只允许 127.0.0.1、::1 或 localhost')
    }
    if (!LOOPBACK_HOSTS.has(url.hostname)) {
      throw new DaemonStateError('A24_BASE_URL 只允许 127.0.0.1、::1 或 localhost')
    }
    return envBase.replace(/\/+$/, '')
  }
  const found = readDaemonState(CONFIG.DAEMON_STATE_FILE)
  if (!found.ok) {
    if (found.reason === 'unreadable') {
      throw new Error('找不到运行中的 agent24d。先启动它（agent24 daemon start）。')
    }
    throw new DaemonStateError(found.message)
  }
  return `http://127.0.0.1:${found.state.port}`
}

/** The bridge bearer comes only from the token file. The state file, when
 * read, supplies the port. A bad token file returns before that read. */
export function discoverDaemon(nowMs = Date.now()): DaemonEndpoint {
  const cred = loadBridgeCredential(nowMs)
  return { base: daemonBase(), token: cred.token }
}
