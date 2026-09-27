// Shared IPC channel names and payload types between main and renderer.
// Capability modules will extend this in M1+.

export const IpcChannels = {
  AppPing: 'app:ping',
  AppVersion: 'app:version',
  ShellOpenExternal: 'shell:open-external',
  BackendProxy: 'backend:proxy',
  OmlxDetect: 'omlx:detect',
  OmlxModels: 'omlx:models',
  OmlxStart: 'omlx:start',
  OmlxStop: 'omlx:stop',
  OmlxWarmup: 'omlx:warmup',
  ModulesList: 'modules:list',
  ModulesDiscover: 'modules:discover',
  ModulesEnable: 'modules:enable',
  ModulesDisable: 'modules:disable',
  ModulesInstall: 'modules:install',
  ModulesUninstall: 'modules:uninstall',
  LlmStatus: 'llm:status',
  // A3-4: push channel only (main -> renderer via webContents.send). Not an
  // ipcMain.handle() target — see main/agentear-events.ts.
  AgentEarEvent: 'agentear:event',
} as const

export type IpcChannel = typeof IpcChannels[keyof typeof IpcChannels]

export type HttpMethod = 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE'

export interface BackendProxyRequest {
  method: HttpMethod
  path: string
  body?: unknown
}

export interface BackendProxyResponse {
  ok: boolean
  status: number
  data: unknown
}

export interface OmlxDetectResult {
  url: string
  apiKey: string
  models: string[]
}

export interface OmlxModelsResult {
  ok: boolean
  models: string[]
  error?: string
}

export interface OmlxStartResult {
  ok: boolean
  url: string
  error?: string
}

export interface OmlxStopResult {
  ok: boolean
}

export interface OmlxWarmupResult {
  model: string
  ok: boolean
  error?: string
}

// ── Module system ─────────────────────────────────────────────────────────────
// MIRROR NOTICE: packages/node-daemon/src/manifest.ts keeps an identical copy;
// machine-readable truth is protocol/module.schema.json. Unified via generated
// api-client in task A6 — keep in lockstep until then.

export type ModuleType = 'ui' | 'headless' | 'hybrid'

export type Permission =
  | 'llm' | 'memory' | 'network' | 'filesystem' | 'wechat' | 'nostr'

export interface ModuleNavItem {
  icon: string
  label: string
  route: string
}

export interface ModuleManifest {
  id: string
  version: string
  name: string
  description: string
  type: ModuleType
  permissions: Permission[]
  navItem?: ModuleNavItem
  /** M3: LLM models this module needs — Gateway will ensure they're loaded on register */
  models?: string[]
  /** M4: OCI container service — BoxLite starts the container and proxies /api/svc/<id>/* to it */
  container?: {
    image: string         // OCI image, e.g. 'python:slim'
    port: number          // guest port inside container
    startCmd?: string[]   // command to start the service (defaults to image entrypoint)
    healthPath?: string   // health-check path polled until 200 (default: '/health')
    memoryMib?: number    // default 512
  }
  /** RESERVED (E5): AgentStore / PGL charter metadata — see protocol/module.schema.json */
  pgl?: Record<string, unknown>
}

// ModuleManifest extended with runtime enable/disable state
export interface ModuleInfo extends ModuleManifest {
  enabled: boolean
}

export interface LlmStatusResult {
  provider: 'omlx' | 'ollama' | 'none'
  url: string
  model: string
}

export interface ModuleInstallResult {
  ok: boolean
  id?: string        // module id if successfully loaded
  error?: string
}

export interface ModuleUninstallResult {
  ok: boolean
  error?: string
}

// ── M4 marketplace: discovery / browse ────────────────────────────────────────
// MIRROR NOTICE: packages/node-daemon/src/module-discovery.ts is the source of
// truth (the daemon computes trust tier from the package name, anti-spoof).

export type TrustTier = 'official' | 'community' | 'third-party'

export interface DiscoveredModule {
  packageName: string
  version: string
  name: string
  description: string
  trustTier: TrustTier
  installed: boolean
}

/** Browse filters driving GET /api/modules/discover?q=&tier=&installed= — all
 * optional; an absent field doesn't constrain. */
export interface DiscoverFilter {
  query?: string
  trustTier?: TrustTier
  installed?: boolean
}

// ── A3 attached modules (docs/design/A3-ATTACHED-MODULE.md) ────────────────────
// MIRROR NOTICE: field names copied from the merged Rust
// `agent24-protocol::types::{AttachedView, AttachedList}` (A3-2a,
// rust/crates/agent24-protocol/src/types.rs). `generation` is NOT part of the
// A3-2a struct yet — it is wired in A3-2b (not merged as of this PR) — so it
// stays optional here and the panel must tolerate its absence.

/** One row of `GET /api/v1/attached` — never carries the token or its hash. */
export interface AttachedView {
  name: string
  manifest_digest: string
  token_id: string
  created_at: string
  /** Open enum: `detached` | `attached` | `disabled` (design §5.1). */
  attach_status: string
  /** A3-2b field (design §3.2); absent against an A3-2a-only daemon. */
  generation?: number
}

export interface AttachedListResponse {
  modules: AttachedView[]
}

/** `agentear.event/1` envelope (AgentEar contracts/schema/agentear.event.v1.schema.json,
 * pinned commit — see apps/desktop/test/fixtures/agentear/SOURCE). This is the
 * payload the kernel relays verbatim inside a WS `type:"module"` event
 * (design §7.1) — Agent24 does not interpret it beyond routing by (session_id, seq). */
export interface AgentEarEventEnvelope {
  schema: string
  event_id: string
  session_id: string
  seq: number
  type: 'turn' | 'transcript' | 'proposal' | 'speech' | 'error' | 'confirm_reply'
  payload: Record<string, unknown>
}
