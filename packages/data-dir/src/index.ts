// The single platform data-directory resolver (ADR-037 decision 3).
// Mirror of rust/crates/agent24-protocol/src/data_dir.rs; both are locked to
// protocol/fixtures/data-dir/cases.json. Desktop re-exports this module;
// other TS packages import it instead of joining `.agent24` themselves.
//
//  1. AGENT24_DATA_DIR (non-empty, absolute on the target platform) wins as-is.
//  2. macOS / Linux: $HOME/.agent24 (unchanged; no XDG migration).
//  3. Windows: %LOCALAPPDATA%\Agent24; missing/empty/relative LOCALAPPDATA is
//     an error, never a fallback to HOME.
//
// Reads env vars directly (not os.homedir()) so it stays in lockstep with the
// daemon, and so tests can point it at an isolated dir.
//
// Published as CommonJS (`dist/index.js`) so Electron's main process and
// node-daemon can `require()` it. tsx / vitest load that same build.

export type DataDirPlatform = 'macos' | 'linux' | 'windows'
export type DataDirErrorCode = 'home_unset' | 'localappdata_unset'

export class DataDirError extends Error {
  constructor(readonly code: DataDirErrorCode) {
    super(
      code === 'home_unset'
        ? 'HOME is not set (or empty); set it or AGENT24_DATA_DIR'
        : 'LOCALAPPDATA is not set (or empty / relative); set it or AGENT24_DATA_DIR',
    )
    this.name = 'DataDirError'
  }
}

export interface DataDirEnv {
  AGENT24_DATA_DIR?: string
  HOME?: string
  LOCALAPPDATA?: string
}

export function currentDataDirPlatform(): DataDirPlatform {
  if (process.platform === 'win32') return 'windows'
  return process.platform === 'darwin' ? 'macos' : 'linux'
}

/** Absolute-ness for the *target* platform, independent of the host. */
function isAbsolute(platform: DataDirPlatform, s: string): boolean {
  if (platform === 'windows') return /^[A-Za-z]:[\\/]/.test(s) || s.startsWith('\\\\')
  return s.startsWith('/')
}

/** Pure resolution; throws DataDirError when the platform root is unknowable. */
export function resolveDataDir(platform: DataDirPlatform, env: DataDirEnv): string {
  const override = env.AGENT24_DATA_DIR
  if (override && isAbsolute(platform, override)) return override
  if (platform === 'windows') {
    const base = env.LOCALAPPDATA
    if (!base || !isAbsolute(platform, base)) throw new DataDirError('localappdata_unset')
    return `${base.replace(/[\\/]+$/, '')}\\Agent24`
  }
  const home = env.HOME
  if (!home) throw new DataDirError('home_unset')
  // Literal concatenation: no `..`/`.` folding (must match the Rust side).
  return `${home.replace(/\/+$/, '')}/.agent24`
}

/** The data root for this process. Throws DataDirError. */
export function dataDir(): string {
  return resolveDataDir(currentDataDirPlatform(), {
    AGENT24_DATA_DIR: process.env['AGENT24_DATA_DIR'],
    HOME: process.env['HOME'],
    LOCALAPPDATA: process.env['LOCALAPPDATA'],
  })
}

/** Like dataDir() but null when unresolvable (callers that treat that as "nothing"). */
export function tryDataDir(): string | null {
  try {
    return dataDir()
  } catch (error) {
    if (error instanceof DataDirError) return null
    throw error
  }
}

/** Literal join for the target platform: no `..`/`.` folding (unlike
 * `path.join`, which would collapse them and read a different file than the
 * Rust side's `PathBuf::join`). Trailing separators of `dir` are trimmed. */
export function joinLiteralFor(platform: DataDirPlatform, dir: string, ...parts: string[]): string {
  const sep = platform === 'windows' ? '\\' : '/'
  const base = dir.replace(platform === 'windows' ? /[\\/]+$/ : /\/+$/, '')
  return [base, ...parts].join(sep)
}

/** [joinLiteralFor] for the host platform. Use for every path derived from
 * `dataDir()` that must agree with the daemon. */
export function joinLiteral(dir: string, ...parts: string[]): string {
  return joinLiteralFor(currentDataDirPlatform(), dir, ...parts)
}
