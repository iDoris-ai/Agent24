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
//
// Loop 9 3c: the source is the remote server or the local WeKnora Lite, one at
// a time (David 11-26). Both drive the same daemon binding 'default' (decision
// 2026-11-27-lite-3c-slot-embedkey, R0: one slot, switched source). The remote
// endpoint, bases and key stay where they are while the local one is in use,
// so switching back finds them as they were. The local origin, default base
// and key come from the running Lite and stay in main's memory: the key is
// pushed as C (`pushCredential`), never written to the login store.

import { randomBytes } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import type { HostResult, KnowledgeHost } from './knowledge-host'
import { isPlainHostname } from './knowledge-hostname'
import type { KnowledgeLoginStore } from './knowledge-logins'
import type { KnowledgeSettingsResult, KnowledgeStatus } from '../shared/ipc-types'
import type { KnowledgeReachability, ProbeInputs } from './knowledge-reachability'

/** The binding the settings drive: one per desktop for now. */
export const KNOWLEDGE_BINDING = 'default'
const FILE = 'knowledge-settings.json'
const VERSION = 1
const MAX_KNOWLEDGE_BASES = 64
const MAX_KNOWLEDGE_BASE_ID_CHARS = 256
/** How long a pushed C counts as valid, and how early it is pushed again. */
const CREDENTIAL_LIFETIME_S = 24 * 3600
const REFRESH_BEFORE_S = 3600

export type KnowledgeSource = 'remote' | 'local'

export interface KnowledgeSettings {
  enabled: boolean
  endpoint: string
  knowledgeBaseIds: string[]
  /** Absent in a file written before Loop 9: remote. */
  source?: KnowledgeSource
}

/** The running local Lite, as main holds it (weknora-lite-host.ts). */
export interface LocalKnowledgeSource {
  credential(): { origin: string; apiKey: string; knowledgeBaseId: string } | null
  onStatus(listener: () => void): () => void
}

/** Where the binding's calls go now, with the key they use. Main-process only. */
interface Target {
  endpoint: string
  knowledgeBaseIds: string[]
  key: string
}

type SettingsResult = KnowledgeSettingsResult

const EMPTY: KnowledgeSettings = { enabled: false, endpoint: '', knowledgeBaseIds: [], source: 'remote' }

/** The daemon's own rule (knowledge_host.rs `endpoint_of`): `https`, or `http`
 * to a loopback IP literal; no user info, query or fragment. Stricter on the
 * host name (isPlainHostname, #1643 PR-Daemon C). */
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
  return scheme && url.username === '' && url.password === '' && url.search === '' && url.hash === '' && isPlainHostname(url.hostname) && url.port !== '0'
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
    && (v['source'] === undefined || v['source'] === 'remote' || v['source'] === 'local')
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
        const { enabled, endpoint, knowledgeBaseIds, source } = record.settings
        return { enabled, endpoint, knowledgeBaseIds: [...knowledgeBaseIds], source: source ?? 'remote' }
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
  /** Loop 9 3c: the local Lite; without it the local source is never complete. */
  local?: LocalKnowledgeSource
}

export class KnowledgeController {
  private settings: KnowledgeSettings
  /** When the C last pushed for the binding expires, or 0 when none is. */
  private pushedUntil = 0
  /** The push under way, if any: the tick never starts a second one beside it. */
  private pushing: Promise<HostResult> | null = null
  /** Bumped by every change; a change whose save finishes under a later
   * number was overtaken and takes no effect. */
  private version = 0
  private saving: Promise<void> = Promise.resolve()
  private readonly nowSeconds: () => number
  private readonly listeners = new Set<() => void>()
  /** Bumped when a new key or endpoint is saved: possibly another principal,
   * so the KnowledgeView must not carry the old page session over. */
  private identity = 0

  constructor(
    private readonly host: KnowledgeHost,
    private readonly logins: KnowledgeLoginStore,
    private readonly store: KnowledgeSettingsStore,
    deps: KnowledgeControllerDeps = {},
  ) {
    this.settings = store.load()
    this.nowSeconds = deps.nowSeconds ?? (() => Math.floor(Date.now() / 1000))
    this.local = deps.local ?? null
    // A Lite that comes up, restarts with a new key or port, or goes away is a
    // new target: a new view partition and the binding brought in line.
    this.local?.onStatus(() => {
      const seen = this.localSeen
      this.localSeen = this.localFingerprint()
      if (this.source() === 'local' && seen !== this.localSeen) {
        this.identity++
        void this.apply()
      }
    })
    this.localSeen = this.localFingerprint()
  }

  private readonly local: LocalKnowledgeSource | null
  /** The Lite's origin, base and key as last seen, to notice a change. */
  private localSeen = ''

  private localFingerprint(): string {
    const c = this.local?.credential()
    return c ? `${c.origin}\u0000${c.knowledgeBaseId}\u0000${c.apiKey}` : ''
  }

  private source(): KnowledgeSource {
    return this.settings.source ?? 'remote'
  }

  /** Where the binding's calls go now: the local Lite while it is the source
   * and running, the saved remote settings otherwise; null when incomplete. */
  private target(): Target | null {
    if (this.source() === 'local') {
      const c = this.local?.credential()
      return c && validEndpoint(c.origin) && validKnowledgeBaseIds([c.knowledgeBaseId])
        ? { endpoint: c.origin, knowledgeBaseIds: [c.knowledgeBaseId], key: c.apiKey }
        : null
    }
    const key = this.key()
    return validEndpoint(this.settings.endpoint) && validKnowledgeBaseIds(this.settings.knowledgeBaseIds) && key !== null
      ? { endpoint: this.settings.endpoint, knowledgeBaseIds: [...this.settings.knowledgeBaseIds], key }
      : null
  }

  /** Loop 9 3c: the endpoint and bases in effect (no key), for main's own
   * callers (upload, the view, entries); null while Off or incomplete. */
  activeTarget(): { endpoint: string; knowledgeBaseIds: string[] } | null {
    const target = this.settings.enabled ? this.target() : null
    return target && { endpoint: target.endpoint, knowledgeBaseIds: target.knowledgeBaseIds }
  }

  status(): KnowledgeStatus {
    return {
      enabled: this.settings.enabled,
      endpoint: this.settings.endpoint,
      knowledgeBaseIds: [...this.settings.knowledgeBaseIds],
      source: this.source(),
      activeKnowledgeBaseIds: this.activeTarget()?.knowledgeBaseIds ?? [],
      hasKey: this.key() !== null,
      active: this.bindingForRun() !== null,
      ...(this.reachability ? { reachability: this.reachability.current() } : {}),
    }
  }

  private reachability: KnowledgeReachability | null = null

  /** K2: the server check this controller reports in `status()`, re-run on every change of settings. */
  attachReachability(reachability: KnowledgeReachability): void {
    this.reachability = reachability
    this.onChange(() => { void reachability.check() })
  }

  /** K2: check now; the status with the result. */
  async checkReachability(): Promise<KnowledgeStatus> {
    await this.reachability?.check()
    return this.status()
  }

  /** K2: what the server check asks with, or null while Off or incomplete. */
  probeInputs(): ProbeInputs | null {
    const target = this.settings.enabled ? this.target() : null
    const knowledgeBaseId = target?.knowledgeBaseIds[0]
    return target && knowledgeBaseId ? { endpoint: target.endpoint, knowledgeBaseId, key: target.key } : null
  }

  /** Called whenever the settings in effect may have changed (KB-C2-03: the
   * KnowledgeView ends with its binding). Returns the unsubscribe. */
  onChange(listener: () => void): () => void {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }

  /** What a KnowledgeView instance stands for: the binding as it is now
   * (endpoint and key included), or none while Off or incomplete. A different
   * value means a new instance in a new partition (KB-C2-03, U4.1/U4.8). */
  viewBinding(): string | null {
    return this.settings.enabled && this.complete() ? `${KNOWLEDGE_BINDING}#${this.identity}` : null
  }

  /**
   * KB-C2-04a (design §6, M2 near-equivalents): what the UI session of the
   * view binding `binding` may use now — C and the generation it stands for —
   * or null once that is not the binding in effect
   * (Off, incomplete, another key or endpoint). An older instance never gets
   * a newer key.
   */
  uiSessionInputs(binding: string): { credential: string; generation: number } | null {
    if (this.viewBinding() !== binding) return null
    const credential = this.target()?.key ?? null
    // U7.1: generations are 1..2^53-1; identity starts at 0 on every launch.
    return credential === null ? null : { credential, generation: this.identity + 1 }
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
    const next: KnowledgeSettings = { enabled: this.settings.enabled, endpoint, knowledgeBaseIds: [...knowledgeBaseIds], source: this.source() }
    try {
      // The key and the settings take their turn with every other write, so
      // an older `enabled` never lands after a later Off; a change overtaken
      // before its turn writes nothing.
      await this.serial(async () => {
        if (overtaken()) return
        const previous = this.logins.list().get(KNOWLEDGE_BINDING)
        if (typeof apiKey === 'string') {
          // The login names the endpoint it is for (`key()`).
          await this.logins.put(KNOWLEDGE_BINDING, { profileId: endpoint, material: apiKey, unusable: false, temporary: false })
          // Counted where the key is written, so a change that is overtaken
          // after this point still moves the view to a new partition.
          this.identity++
        }
        try {
          await this.store.save(next)
        } catch (err) {
          // Not saved: the saved settings keep their own key.
          if (typeof apiKey === 'string') {
            await (previous ? this.logins.put(KNOWLEDGE_BINDING, { ...previous }) : this.logins.remove(KNOWLEDGE_BINDING))
          }
          throw err
        }
      })
    } catch {
      return { ok: false, reason: 'not_saved' }
    }
    if (overtaken()) return { ok: false, reason: 'not_saved' }
    if (next.endpoint !== this.settings.endpoint && this.source() === 'remote') this.identity++
    this.settings = next
    return this.apply()
  }

  /** L7-K2 onboarding: saves the endpoint, its default knowledge base and the
   * issued key, switched On, in one write, and only while no endpoint is
   * configured. The check runs inside the write queue against the saved file,
   * so a user's own save that got there first wins and is never overwritten.
   * `saved: true` means the settings are on disk; `result` is whether the
   * daemon took them now (a daemon not up yet takes them later). */
  async adopt(input: { endpoint: string; knowledgeBaseIds: string[]; apiKey: string }): Promise<
    { saved: true; result: SettingsResult } | { saved: false; reason: 'bound' | 'overtaken' | 'invalid' | 'not_saved' }
  > {
    const { endpoint, knowledgeBaseIds, apiKey } = input
    if (!validEndpoint(endpoint) || !validKnowledgeBaseIds(knowledgeBaseIds) || !/^[\x21-\x7e]{1,16384}$/.test(apiKey)) {
      return { saved: false, reason: 'invalid' }
    }
    // No number is taken up front: a user's change queued before this one must not count as overtaken by it.
    // Any change made after this call (its number moves `version`) makes onboarding stand back.
    const seen = this.version
    let mine = -1
    // The source stays as the user chose it: onboarding fills the remote settings only.
    const next: KnowledgeSettings = { enabled: true, endpoint, knowledgeBaseIds: [...knowledgeBaseIds], source: this.source() }
    let wrote = false
    try {
      await this.serial(async () => {
        if (seen !== this.version || this.settings.endpoint !== '' || this.store.load().endpoint !== '') return
        mine = ++this.version
        const previous = this.logins.list().get(KNOWLEDGE_BINDING)
        await this.logins.put(KNOWLEDGE_BINDING, { profileId: endpoint, material: apiKey, unusable: false, temporary: false })
        this.identity++
        try {
          await this.store.save(next)
        } catch (err) {
          await (previous ? this.logins.put(KNOWLEDGE_BINDING, { ...previous }) : this.logins.remove(KNOWLEDGE_BINDING))
          throw err
        }
        wrote = true
      })
    } catch {
      return { saved: false, reason: 'not_saved' }
    }
    // This write landed but a later change overtook it: what ends up saved is that change's, maybe a snapshot
    // taken before this write. Never `bound` from onboarding's own endpoint: `overtaken`, and onboarding keeps
    // its files to run again.
    if (wrote && mine !== this.version) return { saved: false, reason: 'overtaken' }
    // Nothing written: bound when an endpoint is in place now (the user's own); otherwise another change
    // without one (say, the switch) went first, and onboarding may simply run again.
    if (!wrote) {
      const bound = this.settings.endpoint !== '' || this.store.load().endpoint !== ''
      return { saved: false, reason: bound ? 'bound' : 'overtaken' }
    }
    this.identity++
    this.settings = next
    return { saved: true, result: await this.apply() }
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

  /** Loop 9 3c: the remote server or the local Lite. Choosing local turns
   * Knowledge On; the remote endpoint, bases and key are kept as they are.
   * Takes effect once saved. */
  async setSource(source: KnowledgeSource): Promise<SettingsResult> {
    if (source !== 'remote' && source !== 'local') return { ok: false, reason: 'invalid' }
    const mine = ++this.version
    const next: KnowledgeSettings = {
      ...this.settings,
      knowledgeBaseIds: [...this.settings.knowledgeBaseIds],
      source,
      enabled: source === 'local' ? true : this.settings.enabled,
    }
    try {
      await this.save(next)
    } catch {
      return { ok: false, reason: 'not_saved' }
    }
    if (mine !== this.version) return { ok: false, reason: 'not_saved' }
    // Another source is another server: the view must not carry its page session over.
    if (next.source !== this.source()) this.identity++
    this.settings = next
    this.localSeen = this.localFingerprint()
    return this.apply()
  }

  /** Brings the daemon in line with the settings: the binding activated with
   * its endpoint and a fresh C, or dropped. Also run once at startup. */
  async apply(): Promise<SettingsResult> {
    this.pushedUntil = 0
    for (const listener of this.listeners) {
      try {
        listener()
      } catch (error) {
        console.warn('[knowledge] settings listener failed', error)
      }
    }
    const target = this.settings.enabled ? this.target() : null
    if (!target) {
      return asResult(await this.host.deactivate(KNOWLEDGE_BINDING))
    }
    const { endpoint, knowledgeBaseIds } = target
    const activated = await this.host.activate(KNOWLEDGE_BINDING, { endpoint, knowledgeBaseIds })
    // A daemon not up yet is fine: the host replays the activation and C.
    if (!activated.ok && activated.reason !== 'daemon_unavailable') return asResult(activated)
    return asResult(await this.push())
  }

  /** Called on the main process's sync tick: pushes C again before it runs out. */
  async tick(): Promise<void> {
    // A binding whose last change was refused is not in effect (the host's
    // isActive): the tick does not ask again until the user changes something
    // or the app restarts.
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
    if (this.pushing) return this.pushing
    this.pushing = this.pushOnce()
    try {
      return await this.pushing
    } finally {
      this.pushing = null
    }
  }

  private async pushOnce(): Promise<HostResult> {
    const expiresAt = this.nowSeconds() + CREDENTIAL_LIFETIME_S
    const endpoint = this.settings.endpoint
    let result: HostResult
    if (this.source() === 'local') {
      // The Lite's key is in main's memory only: pushed as it is, never through the login store.
      const target = this.target()
      result = target
        ? await this.host.pushCredential(KNOWLEDGE_BINDING, { kind: 'api_key', value: target.key, expiresAt })
        : { ok: false, reason: 'credential_unavailable' }
    } else {
      result = await this.host.refresh(KNOWLEDGE_BINDING, async (login) => {
        if (login.profileId !== endpoint || this.settings.endpoint !== endpoint || this.source() !== 'remote') {
          throw new Error('key is for another endpoint')
        }
        return { kind: 'api_key', value: login.material, expiresAt }
      })
    }
    // Replayed by the host on a daemon not up yet, or after the backoff of a
    // confirmation not answered in time or not asked at all (#1852 B1).
    // K1: the daemon kept the one it had (same key): refresh by that one's expiry.
    if (result.ok) this.pushedUntil = result.expiresAt ?? expiresAt
    else if (result.reason === 'daemon_unavailable' || result.reason === 'unanswered' || result.reason === 'not_asked') this.pushedUntil = expiresAt
    return result
  }

  private complete(): boolean {
    return this.target() !== null
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
