// The user's Knowledge settings and the one binding they drive (#1085 M3,
// 11-11 thin wiring; decision 2026-10-30-m1-thin-wiring): the WeKnora API
// base, the knowledge bases to search and the On/Off switch live in a plain
// file under userData; the retrieve key lives in the encrypted login store
// (knowledge-logins.ts) and never leaves main. Off is kept here, not in the
// daemon: while Off, the binding is never activated, so the daemon holds no
// endpoint and no C.
//
// Turning Off takes effect at once, in memory and in the daemon, whether or
// not the file is written, and voids every On or change still being saved;
// turning On takes effect only once it is on disk (STATE I1/I2). Off never
// waits behind a refresh (KnowledgeHost.deactivate). Saves are written in
// call order.
//
// The key is kept with the endpoint it was given for, and used for that
// endpoint only: a new address needs the key again, so a renderer cannot send
// a saved key somewhere else.

import { randomBytes } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import type { HostResult, KnowledgeHost } from './knowledge-host'
import type { KnowledgeLoginStore } from './knowledge-logins'
import type { KnowledgeSettingsResult, KnowledgeStatus } from '../shared/ipc-types'

/** The binding the settings drive: one per desktop for now. */
export const KNOWLEDGE_BINDING = 'default'
const FILE = 'knowledge-settings.json'
const VERSION = 1
const MAX_KNOWLEDGE_BASES = 64
const MAX_KNOWLEDGE_BASE_ID_CHARS = 256
/** How long a pushed C counts as valid, and how early it is pushed again. */
const CREDENTIAL_LIFETIME_S = 24 * 3600
const REFRESH_BEFORE_S = 3600

export interface KnowledgeSettings {
  enabled: boolean
  endpoint: string
  knowledgeBaseIds: string[]
}

type SettingsResult = KnowledgeSettingsResult

const EMPTY: KnowledgeSettings = { enabled: false, endpoint: '', knowledgeBaseIds: [] }

/** The daemon's own rule (knowledge_host.rs `endpoint_of`): `https`, or `http`
 * to a loopback IP literal; no user info, query or fragment. */
export function validEndpoint(value: unknown): value is string {
  if (typeof value !== 'string') return false
  let url: URL
  try {
    url = new URL(value)
  } catch {
    return false
  }
  const loopback = url.hostname === '[::1]' || /^127(\.\d{1,3}){3}$/.test(url.hostname)
  const scheme = url.protocol === 'https:' || (url.protocol === 'http:' && loopback)
  return scheme && url.username === '' && url.password === '' && url.search === '' && url.hash === ''
}

export function validKnowledgeBaseIds(value: unknown): value is string[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_KNOWLEDGE_BASES) return false
  const ok = value.every((id) => typeof id === 'string' && id.trim() !== '' && [...id].length <= MAX_KNOWLEDGE_BASE_ID_CHARS)
  return ok && new Set(value).size === value.length
}

function isSettings(value: unknown): value is KnowledgeSettings {
  if (typeof value !== 'object' || value === null) return false
  const v = value as Record<string, unknown>
  return typeof v['enabled'] === 'boolean' && typeof v['endpoint'] === 'string' && Array.isArray(v['knowledgeBaseIds'])
    && v['knowledgeBaseIds'].every((id) => typeof id === 'string')
}

export class KnowledgeSettingsStore {
  private readonly file: string

  constructor(dir: string) {
    this.file = path.join(dir, FILE)
  }

  /** The saved settings; a missing or unreadable file reads as Off. */
  load(): KnowledgeSettings {
    try {
      const record = JSON.parse(fs.readFileSync(this.file, 'utf8')) as { v?: unknown; settings?: unknown }
      if (record?.v === VERSION && isSettings(record.settings)) {
        const { enabled, endpoint, knowledgeBaseIds } = record.settings
        return { enabled, endpoint, knowledgeBaseIds: [...knowledgeBaseIds] }
      }
    } catch {
      // Missing or unreadable: Off.
    }
    return { ...EMPTY, knowledgeBaseIds: [] }
  }

  async save(settings: KnowledgeSettings): Promise<void> {
    const tmp = `${this.file}.${randomBytes(8).toString('hex')}.tmp`
    await fs.promises.mkdir(path.dirname(this.file), { recursive: true, mode: 0o700 })
    try {
      await fs.promises.writeFile(tmp, JSON.stringify({ v: VERSION, settings }), { mode: 0o600, flag: 'wx' })
      await fs.promises.rename(tmp, this.file)
    } catch (err) {
      await fs.promises.rm(tmp, { force: true })
      throw err
    }
  }
}

export interface KnowledgeControllerDeps {
  /** Unix seconds. */
  nowSeconds?: () => number
}

export class KnowledgeController {
  private settings: KnowledgeSettings
  /** When the C last pushed for the binding expires, or 0 when none is. */
  private pushedUntil = 0
  /** Bumped by every change; a change whose save finishes under a later
   * number was overtaken and takes no effect. */
  private version = 0
  private saving: Promise<void> = Promise.resolve()
  private readonly nowSeconds: () => number

  constructor(
    private readonly host: KnowledgeHost,
    private readonly logins: KnowledgeLoginStore,
    private readonly store: KnowledgeSettingsStore,
    deps: KnowledgeControllerDeps = {},
  ) {
    this.settings = store.load()
    this.nowSeconds = deps.nowSeconds ?? (() => Math.floor(Date.now() / 1000))
  }

  status(): KnowledgeStatus {
    return {
      ...this.settings,
      knowledgeBaseIds: [...this.settings.knowledgeBaseIds],
      hasKey: this.key() !== null,
      active: this.bindingForRun() !== null,
    }
  }

  /** The binding a new run is to name, or none while Off or incomplete. */
  bindingForRun(): string | null {
    return this.settings.enabled && this.complete() && this.host.isActive(KNOWLEDGE_BINDING) ? KNOWLEDGE_BINDING : null
  }

  /** Saves the endpoint, the knowledge bases and, when given, a new key; the
   * switch stays as it is. A change while On takes effect once saved. */
  async configure(input: { endpoint: unknown; knowledgeBaseIds: unknown; apiKey?: unknown }): Promise<SettingsResult> {
    const { endpoint, knowledgeBaseIds, apiKey } = input
    const keyOk = apiKey === undefined || (typeof apiKey === 'string' && /^[\x21-\x7e]{1,16384}$/.test(apiKey))
    if (!validEndpoint(endpoint) || !validKnowledgeBaseIds(knowledgeBaseIds) || !keyOk) return { ok: false, reason: 'invalid' }
    const mine = ++this.version
    const overtaken = () => mine !== this.version
    const next: KnowledgeSettings = { enabled: this.settings.enabled, endpoint, knowledgeBaseIds: [...knowledgeBaseIds] }
    try {
      // The key and the settings take their turn with every other write, so
      // an older `enabled` never lands after a later Off; a change overtaken
      // before its turn writes nothing.
      await this.serial(async () => {
        if (overtaken()) return
        if (typeof apiKey === 'string') {
          // The login names the endpoint it is for (`key()`).
          await this.logins.put(KNOWLEDGE_BINDING, { profileId: endpoint, material: apiKey, unusable: false, temporary: false })
        }
        await this.store.save(next)
      })
    } catch {
      return { ok: false, reason: 'not_saved' }
    }
    if (overtaken()) return { ok: false, reason: 'not_saved' }
    this.settings = next
    return this.apply()
  }

  /** Off at once, saved or not; On once saved. */
  async setEnabled(enabled: boolean): Promise<SettingsResult> {
    const mine = ++this.version
    const next = { ...this.settings, knowledgeBaseIds: [...this.settings.knowledgeBaseIds], enabled }
    if (!enabled) {
      this.settings = next
      const saved = this.save(next).then(() => true, () => false)
      const dropped = await this.apply()
      if (!(await saved)) return { ok: false, reason: 'not_saved' }
      return dropped
    }
    try {
      await this.save(next)
    } catch {
      return { ok: false, reason: 'not_saved' }
    }
    if (mine !== this.version) return { ok: false, reason: 'not_saved' }
    this.settings = next
    return this.apply()
  }

  /** Brings the daemon in line with the settings: the binding activated with
   * its endpoint and a fresh C, or dropped. Also run once at startup. */
  async apply(): Promise<SettingsResult> {
    this.pushedUntil = 0
    if (!this.settings.enabled || !this.complete()) {
      return asResult(await this.host.deactivate(KNOWLEDGE_BINDING))
    }
    const { endpoint, knowledgeBaseIds } = this.settings
    const activated = await this.host.activate(KNOWLEDGE_BINDING, { endpoint, knowledgeBaseIds })
    // A daemon not up yet is fine: the host replays the activation and C.
    if (!activated.ok && activated.reason !== 'daemon_unavailable') return asResult(activated)
    return asResult(await this.push())
  }

  /** Called on the main process's sync tick: pushes C again before it runs out. */
  async tick(): Promise<void> {
    if (this.bindingForRun() === null) return
    if (this.pushedUntil - this.nowSeconds() > REFRESH_BEFORE_S) return
    await this.push()
  }

  private save(settings: KnowledgeSettings): Promise<void> {
    return this.serial(() => this.store.save(settings))
  }

  /** Writes run one at a time, in call order. */
  private serial(write: () => Promise<void>): Promise<void> {
    const run = this.saving.then(write)
    this.saving = run.catch(() => {})
    return run
  }

  private async push(): Promise<HostResult> {
    const expiresAt = this.nowSeconds() + CREDENTIAL_LIFETIME_S
    const endpoint = this.settings.endpoint
    const result = await this.host.refresh(KNOWLEDGE_BINDING, async (login) => {
      if (login.profileId !== endpoint || this.settings.endpoint !== endpoint) throw new Error('key is for another endpoint')
      return { kind: 'api_key', value: login.material, expiresAt }
    })
    // Replayed by the host on a daemon not up yet.
    if (result.ok || result.reason === 'daemon_unavailable') this.pushedUntil = expiresAt
    return result
  }

  private complete(): boolean {
    return validEndpoint(this.settings.endpoint) && validKnowledgeBaseIds(this.settings.knowledgeBaseIds) && this.key() !== null
  }

  /** The saved key, if it was given for the endpoint in the settings. */
  private key(): string | null {
    const login = this.logins.list().get(KNOWLEDGE_BINDING)
    return login && !login.unusable && login.profileId === this.settings.endpoint ? login.material : null
  }
}

function asResult(result: HostResult): SettingsResult {
  if (result.ok) return { ok: true }
  return { ok: false, reason: result.reason === 'daemon_unavailable' ? 'unavailable' : 'not_saved' }
}
