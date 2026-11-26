// KB-LITE-02 (Loop 9, 3b): the App's one WeKnora Lite. Main-process only.
// 3c drives it: start() when the user picks the local knowledge base (and on
// launch when it is picked), stop() when they switch away; quit calls
// shutdown(). status() and credential() are what 3c reads.
//
// Crash recovery (main flow only): SidecarManager never restarts by itself.
// When the Lite fails after it was started, and the manager has finished
// reaping it, start() is called again after 1, 2, 4, 8, 16 s. A sixth failure
// in a row gives up: the state reads `stopped` with `gaveUp` until 3c's
// "retry" calls start() again. Reaching `healthy` resets the count.

import { spawn } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'

import { dataDir, joinLiteral } from './data-dir'
import {
  defaultWeknoraLiteComponentsRoot,
  quickValidateWeknoraLiteInstall,
  verifyInstalledWeknoraLiteBinary,
  weknoraLiteInstallDir,
} from './weknora-lite-component'
import { WEKNORA_LITE_PIN } from './weknora-lite-pin'
import { SidecarManager, exactTreeStopper, type SidecarLogger } from './sidecar-manager'
import { MemoryEndpointHandoff, type SidecarState } from './sidecar-contract'
import {
  WeknoraLiteHealth,
  WeknoraLiteLauncher,
  ensureWeknoraLiteDataDir,
  validateWeknoraLiteEmbedding,
  weknoraLiteSidecarSpec,
  writeWeknoraLiteModelsConfig,
  WEKNORA_LITE_READY_PHASE_TIMEOUT_MS,
  type WeknoraLiteEmbedding,
  type WeknoraLiteHandshake,
  type WeknoraLiteLaunchPlan,
} from './weknora-lite-sidecar'

export const WEKNORA_LITE_RESTART_DELAYS_MS: readonly number[] = [1_000, 2_000, 4_000, 8_000, 16_000]

export function defaultWeknoraLiteDataDir(): string {
  return joinLiteral(dataDir(), 'data', 'weknora-lite')
}

/** The pinned program installed by 3a, re-hashed before every launch. Throws when not installed. */
export function installedWeknoraLiteProgram(): { binary: string; dir: string } {
  const dir = weknoraLiteInstallDir(defaultWeknoraLiteComponentsRoot(), WEKNORA_LITE_PIN)
  if (!quickValidateWeknoraLiteInstall(dir, WEKNORA_LITE_PIN)) throw new Error('WeKnora Lite is not installed')
  return { binary: verifyInstalledWeknoraLiteBinary(dir, WEKNORA_LITE_PIN), dir }
}

export interface WeknoraLiteHostStatus {
  readonly state: SidecarState
  /** The automatic restarts ran out; only an explicit start() tries again. */
  readonly gaveUp: boolean
  /** Why the last start or run failed (English, no secrets). */
  readonly error?: string
  /** Loopback origin, while running. */
  readonly origin?: string
  readonly knowledgeBaseId?: string
}

/** Main-process only: never serialize to a renderer or a log. */
export interface WeknoraLiteCredential {
  readonly origin: string
  readonly apiKey: string
  readonly knowledgeBaseId: string
  readonly expiresAt: string | null
}

export interface WeknoraLiteHostDeps {
  /** The installed, verified program: `verifyInstalledWeknoraLiteBinary` + its dir (3a). Throws when not installed. */
  readonly program: () => { binary: string; dir: string }
  /** The embedding the Lite indexes with (iDoris gateway + an embed-only key; 3c). */
  readonly embedding: () => Promise<WeknoraLiteEmbedding>
  readonly dataDir?: string
  readonly logPath?: string
  readonly environment?: NodeJS.ProcessEnv
  readonly spawnFn?: typeof spawn
  readonly fetchFn?: typeof fetch
  readonly logger?: SidecarLogger
  readonly platform?: NodeJS.Platform
  readonly kill?: (pid: number, signal: NodeJS.Signals | 0) => unknown
  readonly readyPhaseTimeoutMs?: number
  readonly restartDelaysMs?: readonly number[]
}

const consoleLogger: SidecarLogger = {
  info: (event, fields) => console.info(`[weknora-lite] ${event}`, fields ?? ''),
  warn: (event, fields) => console.warn(`[weknora-lite] ${event}`, fields ?? ''),
}

const message = (error: unknown) => (error instanceof Error ? error.message : String(error)).slice(0, 256)

export class WeknoraLiteHost {
  private readonly manager: SidecarManager
  private readonly handoff = new MemoryEndpointHandoff()
  private readonly listeners = new Set<(status: WeknoraLiteHostStatus) => void>()
  private readonly delays: readonly number[]
  private readonly logger: SidecarLogger
  private readonly dataDirPath: string
  private plan: WeknoraLiteLaunchPlan | null = null
  private handshake: WeknoraLiteHandshake | null = null
  /** True between start() and stop()/shutdown()/giving up: failures then restart. */
  private wanted = false
  private closed = false
  private restarts = 0
  private timer: ReturnType<typeof setTimeout> | null = null
  private gaveUp = false
  private error: string | undefined
  private preparing = false

  constructor(private readonly deps: WeknoraLiteHostDeps) {
    this.logger = deps.logger ?? consoleLogger
    this.delays = deps.restartDelaysMs ?? WEKNORA_LITE_RESTART_DELAYS_MS
    this.dataDirPath = deps.dataDir ?? defaultWeknoraLiteDataDir()
    const fetchFn = deps.fetchFn ?? fetch
    this.manager = new SidecarManager(
      weknoraLiteSidecarSpec(deps.readyPhaseTimeoutMs ?? WEKNORA_LITE_READY_PHASE_TIMEOUT_MS),
      new WeknoraLiteLauncher(
        () => {
          if (!this.plan) throw new Error('WeKnora Lite launch plan is unavailable')
          return this.plan
        },
        (handshake) => { this.handshake = handshake },
        deps.spawnFn ?? spawn,
        fetchFn,
        this.logger,
        deps.platform,
        deps.kill,
      ),
      new WeknoraLiteHealth(() => this.handshake?.knowledgeBaseId ?? null, fetchFn),
      exactTreeStopper,
      this.handoff,
      this.logger,
    )
    this.manager.onStatus((status) => this.onManagerStatus(status.state))
  }

  status(): WeknoraLiteHostStatus {
    const state = this.preparing ? 'starting' : this.manager.status().state
    const live = this.liveCredential()
    return {
      state,
      gaveUp: this.gaveUp,
      ...(this.error ? { error: this.error } : {}),
      ...(live ? { origin: live.origin, knowledgeBaseId: live.knowledgeBaseId } : {}),
    }
  }

  /** The running instance's key and default KB, or null when it is not up. Main-process only. */
  credential(): WeknoraLiteCredential | null {
    return this.liveCredential()
  }

  onStatus(listener: (status: WeknoraLiteHostStatus) => void): () => void {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }

  /** Start (or retry after giving up). Resolves once ready or failed; never throws. */
  async start(): Promise<WeknoraLiteHostStatus> {
    if (this.closed) return this.status()
    this.wanted = true
    this.gaveUp = false
    this.restarts = 0
    this.clearTimer()
    await this.attempt()
    return this.status()
  }

  async stop(): Promise<WeknoraLiteHostStatus> {
    this.wanted = false
    this.clearTimer()
    try {
      await this.manager.stop()
    } catch (error) {
      // The tree did not empty in time; the manager keeps it `failed` and owned.
      this.logger.warn('weknora-lite.stop_failed', { error: message(error) })
    }
    this.handshake = null
    this.emit()
    return this.status()
  }

  /** App quit: stop and reap the process tree; nothing starts again. */
  async shutdown(): Promise<void> {
    this.closed = true
    await this.stop()
  }

  private liveCredential(): WeknoraLiteCredential | null {
    const current = this.handoff.current()
    const handshake = this.handshake
    if (!current || !handshake || current.endpoint.secret !== handshake.apiKey) return null
    return { origin: handshake.origin, apiKey: handshake.apiKey, knowledgeBaseId: handshake.knowledgeBaseId, expiresAt: handshake.expiresAt }
  }

  private async attempt(): Promise<void> {
    if (this.closed || !this.wanted) return
    this.preparing = true
    this.emit()
    try {
      this.plan = await this.prepare()
    } catch (error) {
      this.preparing = false
      this.fail(message(error))
      return
    }
    this.preparing = false
    if (this.closed || !this.wanted) return
    try {
      const status = await this.manager.start()
      // Still `failed` without having launched: the last instance is being reaped. Try again later.
      if (status.state === 'failed') this.schedule(false)
      else if (status.state !== 'stopped') this.error = undefined
    } catch (error) {
      // The manager already moved to `failed`; onManagerStatus scheduled the restart.
      this.error = message(error)
      this.logger.warn('weknora-lite.start_failed', { error: this.error })
      this.emit()
    }
  }

  private async prepare(): Promise<WeknoraLiteLaunchPlan> {
    const program = this.deps.program()
    const embedding = await this.deps.embedding()
    validateWeknoraLiteEmbedding(embedding)
    ensureWeknoraLiteDataDir(this.dataDirPath)
    const logPath = this.deps.logPath ?? path.join(dataDir(), 'logs', 'weknora-lite.log')
    fs.mkdirSync(path.dirname(logPath), { recursive: true })
    return {
      binary: program.binary,
      cwd: program.dir,
      stateDir: this.dataDirPath,
      modelsConfigPath: writeWeknoraLiteModelsConfig(this.dataDirPath, embedding),
      logPath,
      embedding,
      environment: this.deps.environment ?? process.env,
      readyPhaseTimeoutMs: this.deps.readyPhaseTimeoutMs ?? WEKNORA_LITE_READY_PHASE_TIMEOUT_MS,
    }
  }

  private onManagerStatus(state: SidecarState): void {
    if (state === 'healthy') this.restarts = 0
    if (state === 'failed') {
      this.handshake = null
      if (this.wanted && !this.closed) this.schedule(true)
    }
    this.emit()
  }

  private fail(reason: string): void {
    this.error = reason
    this.logger.warn('weknora-lite.prepare_failed', { error: reason })
    this.schedule(true)
    this.emit()
  }

  /** `counts`: a real failure uses up one restart; waiting for the reaping does not. */
  private schedule(counts: boolean): void {
    if (this.timer || this.closed || !this.wanted) return
    if (counts && this.restarts >= this.delays.length) {
      this.wanted = false
      this.gaveUp = true
      this.logger.warn('weknora-lite.gave_up', { restarts: this.restarts })
      // Back to `stopped` once the failed instance is reaped.
      void this.manager.stop().finally(() => this.emit())
      return
    }
    const delay = this.delays[Math.min(this.restarts, this.delays.length - 1)]
    if (counts) this.restarts += 1
    this.timer = setTimeout(() => {
      this.timer = null
      void this.attempt()
    }, delay)
    this.timer.unref?.()
  }

  private clearTimer(): void {
    if (this.timer) clearTimeout(this.timer)
    this.timer = null
  }

  private emit(): void {
    const snapshot = this.status()
    for (const listener of [...this.listeners]) {
      try { listener(snapshot) } catch { /* a listener cannot disturb the host */ }
    }
  }
}
