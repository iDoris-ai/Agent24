// Loop 9 3c: the local knowledge base (WeKnora Lite) as the user turns it on
// in the Knowledge settings. Install (3a) when it is not there yet, then launch
// (3b); the binding follows the running Lite on its own (knowledge-settings.ts,
// `local`). Main-process only: the renderer sees `LiteKnowledgeStatus`, never a
// path, a port or a key.
//
// No upgrade, migration or backup (David 11-26): one pinned Lite, installed
// once. The Lite's embedding goes through the iDoris gateway with an embed-only
// key the daemon issues once (POST /api/v1/idoris/lite-embedding, ProductHost).

import http from 'node:http'
import type { DaemonConnection } from './backend-manager'
import type { WeknoraLiteEmbedding } from './weknora-lite-sidecar'
import type { WeknoraLiteHostStatus } from './weknora-lite-host'
import type { WeknoraLiteStatus } from './weknora-lite-component'
import type { LiteKnowledgeStatus } from '../shared/ipc-types'

export const LITE_EMBEDDING_PATH = '/api/v1/idoris/lite-embedding'
const MAX_RESPONSE_BYTES = 16 * 1024

/** Thrown when the daemon cannot give the Lite an embedding: iDoris not
 * running, no embed role, or no daemon. The message is a code, never a key. */
export class LiteEmbeddingUnavailable extends Error {}

/** Asks the daemon for the Lite's embedding. `connection` carries the
 * ProductHost credential; the answer stays in main. */
export function fetchLiteEmbedding(connection: DaemonConnection | null, timeoutMs = 15_000): Promise<WeknoraLiteEmbedding> {
  if (!connection) return Promise.reject(new LiteEmbeddingUnavailable('daemon_unavailable'))
  return new Promise((resolve, reject) => {
    const req = http.request({
      host: '127.0.0.1',
      port: connection.port,
      method: 'POST',
      path: LITE_EMBEDDING_PATH,
      headers: { ...connection.authHeaders, 'Content-Length': 0 },
      timeout: timeoutMs,
    }, (res) => {
      const chunks: Buffer[] = []
      let size = 0
      res.on('data', (chunk: Buffer) => {
        size += chunk.length
        if (size > MAX_RESPONSE_BYTES) req.destroy(new LiteEmbeddingUnavailable('invalid_response'))
        else chunks.push(chunk)
      })
      res.on('error', () => reject(new LiteEmbeddingUnavailable('daemon_unavailable')))
      res.on('end', () => {
        if (res.statusCode !== 200) {
          reject(new LiteEmbeddingUnavailable('idoris_unavailable'))
          return
        }
        try {
          const body = JSON.parse(Buffer.concat(chunks).toString('utf8')) as Record<string, unknown>
          const { base_url: baseUrl, api_key: apiKey, model, dimension } = body
          if (typeof baseUrl !== 'string' || typeof apiKey !== 'string' || typeof model !== 'string' || typeof dimension !== 'number') {
            throw new Error('shape')
          }
          resolve({ baseUrl, apiKey, model, dimension })
        } catch {
          reject(new LiteEmbeddingUnavailable('invalid_response'))
        }
      })
    })
    req.on('error', (error) => reject(error instanceof LiteEmbeddingUnavailable ? error : new LiteEmbeddingUnavailable('daemon_unavailable')))
    req.on('timeout', () => req.destroy(new LiteEmbeddingUnavailable('daemon_unavailable')))
    req.end()
  })
}

export interface LiteKnowledgeDeps {
  installer: {
    status(): WeknoraLiteStatus
    install(): Promise<WeknoraLiteStatus>
    onProgress(listener: (status: WeknoraLiteStatus) => void): () => void
  }
  host: {
    status(): WeknoraLiteHostStatus
    start(): Promise<WeknoraLiteHostStatus>
    stop(): Promise<WeknoraLiteHostStatus>
    onStatus(listener: (status: WeknoraLiteHostStatus) => void): () => void
  }
  /** Why the last embedding request failed, if it did (set by the embedding source). */
  embeddingProblem?: () => string | null
}

/** The local knowledge base's lifecycle as the settings page sees it. */
export class LiteKnowledge {
  /** Between enable() and disable(): the Lite should be installed and running. */
  private wanted = false
  /** The run of install-then-start under way, so a second click joins it. */
  private running: Promise<void> | null = null
  /** Bumped by disable(): a run started before it has been stopped and is not joined. */
  private epoch = 0
  private runningEpoch = -1

  constructor(private readonly deps: LiteKnowledgeDeps) {}

  status(): LiteKnowledgeStatus {
    if (!this.wanted) return { phase: 'off' }
    const install = this.deps.installer.status()
    switch (install.state) {
      case 'unavailable':
        return { phase: 'unavailable' }
      case 'not-installed':
        return { phase: 'not_installed', total: install.total }
      case 'downloading':
        return { phase: 'downloading', received: install.received ?? 0, total: install.total }
      case 'verifying':
        return { phase: 'verifying' }
      case 'failed':
        return { phase: 'install_failed', failure: install.failure ?? 'other' }
      case 'installed':
        break
    }
    const host = this.deps.host.status()
    if (host.state === 'healthy' || host.state === 'ready' || host.state === 'degraded') return { phase: 'ready' }
    if (host.gaveUp || host.state === 'failed') {
      const problem = this.deps.embeddingProblem?.() ?? null
      return { phase: 'failed', reason: problem ? 'embedding_unavailable' : 'start_failed' }
    }
    return { phase: 'starting' }
  }

  /** L11-1: this build can have the Lite: it has a download source, or the Lite is already installed.
   * False exactly when the installer reports 'unavailable'. Does not depend on enable()/disable(). */
  offered(): boolean {
    return this.deps.installer.status().state !== 'unavailable'
  }

  /** Turn the local knowledge base on: install if needed, then launch. Also
   * the retry after an install failed or the Lite gave up. Never throws. */
  enable(): Promise<void> {
    this.wanted = true
    if (this.running && this.runningEpoch === this.epoch) return this.running
    // A run from before a disable() was told to stop: start again once it is over (Codex C2).
    const previous = this.running ?? Promise.resolve()
    const run = previous.then(() => this.installThenStart()).finally(() => {
      if (this.running === run) this.running = null
    })
    this.running = run
    this.runningEpoch = this.epoch
    return run
  }

  /** Back to the remote source: the Lite stops; what it installed and stored stays. */
  async disable(): Promise<void> {
    this.wanted = false
    this.epoch++
    await this.deps.host.stop()
  }

  private async installThenStart(): Promise<void> {
    try {
      let install = this.deps.installer.status()
      if (install.state !== 'installed') install = await this.deps.installer.install()
      if (install.state !== 'installed' || !this.wanted) return
      await this.deps.host.start()
    } catch (error) {
      // install() and start() report through their status; this is only a guard.
      console.warn('[knowledge-lite] enable failed', error instanceof Error ? error.message : String(error))
    }
  }
}
