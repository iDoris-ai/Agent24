// Long-term Knowledge login state (KB-HOST-01 PR-B2a; #1085 design v2 §1,
// UI-BRIDGE U4.9–U4.11). One file, encrypted whole with Electron safeStorage:
// either it decrypts and parses, and the host can list every login it keeps,
// or it does not, and every login counts as gone (the user signs in again).
// The decrypted material never reaches a log line or any renderer IPC.
//
// Tightening changes (marking a login unusable, removing it) take effect in
// memory at once and stay in effect when the write fails; adding or replacing
// a login takes effect only once it is on disk (STATE I1/I2).

import fs from 'node:fs'
import path from 'node:path'

export interface KnowledgeLogin {
  /** The profile the login was made for, recorded by the host itself (U4.9). */
  profileId: string
  /** Refresh token or device grant: what gets a new credential without the user. */
  material: string
  /** Set first when a login is being destroyed (U4.11 ①); never used again. */
  unusable: boolean
  /** Made before its binding was persisted (U4.9); destroyed if that fails. */
  temporary: boolean
}

/** The part of Electron's `safeStorage` this module uses. */
export interface SecretBox {
  isEncryptionAvailable(): boolean
  encryptString(plain: string): Buffer
  decryptString(cipher: Buffer): string
  getSelectedStorageBackend?(): string
}

export type LoadResult =
  | { ok: true }
  | { ok: false; reason: 'unreadable' | 'encryption_unavailable' }

const FILE = 'knowledge-logins.bin'
const VERSION = 1

/** On Linux, safeStorage may "encrypt" with a hard-coded key (`basic_text`). */
export function encryptionUsable(box: SecretBox, platform: NodeJS.Platform = process.platform): boolean {
  if (!box.isEncryptionAvailable()) return false
  if (platform !== 'linux') return true
  const backend = box.getSelectedStorageBackend?.() ?? 'unknown'
  return backend !== 'basic_text' && backend !== 'unknown'
}

function isLogin(value: unknown): value is KnowledgeLogin {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false
  const v = value as Record<string, unknown>
  return (
    Object.keys(v).length === 4 &&
    typeof v['profileId'] === 'string' &&
    typeof v['material'] === 'string' &&
    typeof v['unusable'] === 'boolean' &&
    typeof v['temporary'] === 'boolean'
  )
}

export class KnowledgeLoginStore {
  private readonly file: string
  private logins = new Map<string, KnowledgeLogin>()
  private writable = false
  private queue: Promise<void> = Promise.resolve()

  constructor(
    dir: string,
    private readonly box: SecretBox,
    private readonly platform: NodeJS.Platform = process.platform,
  ) {
    this.file = path.join(dir, FILE)
  }

  /** Reads the file once at startup. Until it succeeds nothing is listed. */
  load(): LoadResult {
    this.logins = new Map()
    this.writable = false
    fs.rmSync(`${this.file}.tmp`, { force: true })
    if (!encryptionUsable(this.box, this.platform)) return { ok: false, reason: 'encryption_unavailable' }
    // From here on a write replaces whatever is on disk, readable or not.
    this.writable = true
    if (!fs.existsSync(this.file)) return { ok: true }
    try {
      const parsed = JSON.parse(this.box.decryptString(fs.readFileSync(this.file))) as unknown
      const record = parsed as { v?: unknown; logins?: unknown }
      const logins = record?.logins
      if (record?.v !== VERSION || typeof logins !== 'object' || logins === null || Array.isArray(logins)) {
        throw new Error()
      }
      const entries = Object.entries(logins)
      if (!entries.every(([, login]) => isLogin(login))) throw new Error()
      this.logins = new Map(entries as [string, KnowledgeLogin][])
      return { ok: true }
    } catch {
      return { ok: false, reason: 'unreadable' }
    }
  }

  /** Every login kept, by binding id. */
  list(): ReadonlyMap<string, Readonly<KnowledgeLogin>> {
    return new Map(this.logins)
  }

  /** Adds or replaces the login of `bindingId` once it is on disk. */
  put(bindingId: string, login: KnowledgeLogin): Promise<void> {
    return this.enqueue(async () => {
      const next = new Map(this.logins).set(bindingId, { ...login })
      await this.write(next)
      this.logins = next
    })
  }

  markUnusable(bindingId: string): Promise<void> {
    const login = this.logins.get(bindingId)
    if (login) this.logins.set(bindingId, { ...login, unusable: true })
    return this.enqueue(() => this.write(this.logins))
  }

  remove(bindingId: string): Promise<void> {
    this.logins.delete(bindingId)
    return this.enqueue(() => this.write(this.logins))
  }

  /** Writes run one at a time, in call order; a failed one does not stop the next. */
  private enqueue(task: () => Promise<void>): Promise<void> {
    const run = this.queue.then(task)
    this.queue = run.catch(() => {})
    return run
  }

  private async write(logins: Map<string, KnowledgeLogin>): Promise<void> {
    if (!this.writable || !encryptionUsable(this.box, this.platform)) {
      throw new Error('Knowledge logins cannot be saved: secure storage is unavailable')
    }
    const plain = JSON.stringify({ v: VERSION, logins: Object.fromEntries(logins) })
    const tmp = `${this.file}.tmp`
    await fs.promises.mkdir(path.dirname(this.file), { recursive: true })
    await fs.promises.writeFile(tmp, this.box.encryptString(plain), { mode: 0o600 })
    await fs.promises.rename(tmp, this.file)
  }
}
