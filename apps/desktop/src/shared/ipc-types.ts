// Shared IPC channel names and payload types between main and renderer.
// Capability modules will extend this in M1+.

export const IpcChannels = {
  AppPing: 'app:ping',
  AppVersion: 'app:version',
  ShellOpenExternal: 'shell:open-external',
  BackendProxy: 'backend:proxy',
  BackendEndpoint: 'backend:endpoint',
  CreativeShow: 'creative:show',
  CreativeRestart: 'creative:restart',
  CreativeBounds: 'creative:bounds',
  CreativeHide: 'creative:hide',
  // Owner decision 2026-10-05: Open Design ships as an on-demand component,
  // not bundled in the installer. These channels manage its lifecycle
  // (download + verify + install) independently of CreativeServeWeb, which
  // only starts the already-installed component.
  OpenDesignComponentStatus: 'open-design-component:status',
  OpenDesignComponentInstall: 'open-design-component:install',
  // Push channel only (main -> renderer), mirrors AgentEarEvent's pattern.
  OpenDesignComponentProgress: 'open-design-component:progress',
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
  IdorisStatus: 'idoris:status',
  // A3-4: push channel only (main -> renderer via webContents.send). Not an
  // ipcMain.handle() target — see main/agentear-events.ts.
  AgentEarEvent: 'agentear:event',
  // A3-4 review M5: pull channel — the panel calls this once on mount to get
  // the main-process log's current state (main/agentear-log.ts), so
  // navigating away and back doesn't lose it. Ordinary ipcMain.handle().
  AgentEarSnapshot: 'agentear:snapshot',
  // ME4-desktop-model-ui: push/pull pair for the kernel's `model.call` WS
  // events (agent24d/src/model_callback.rs), mirroring AgentEarEvent /
  // AgentEarSnapshot's shape exactly — see main/model-call-log.ts.
  ModelCallEvent: 'model-call:event',
  ModelCallSnapshot: 'model-call:snapshot',
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

/** FU-93: the backend daemon's actual (dynamic) port, so the sidebar can show
 * it instead of a hardcoded guess. `null` while the daemon has not announced
 * its ready line yet. */
export interface BackendEndpointResult {
  port: number
}

export interface CreativeViewBounds {
  x: number
  y: number
  width: number
  height: number
}

export interface CreativeViewResult {
  ok: boolean
  origin?: string
  error?: string
  /** Set instead of a generic error when the on-demand Open Design
   * component is not installed yet — the renderer should show the download
   * card (driven by the OpenDesignComponent* IPC) rather than an error. */
  needsDownload?: boolean
  size?: number
}

/** Mirrors main/open-design-component.ts's `OpenDesignComponentStatus` —
 * duplicated here (not imported) because this file is the main/renderer
 * boundary and must stay free of main-process-only modules. */
export interface OpenDesignComponentStatusResult {
  state: 'not-installed' | 'downloading' | 'verifying' | 'installed' | 'failed' | 'unavailable'
  received?: number
  total?: number
  size?: number
  dir?: string
  error?: string
  reason?: string
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

export interface IdorisCapacityEntry {
  id: string
  capability: string
  resident: boolean
  estimated_memory_gb: number
  ctx_limit: number
  queue_depth: number
  admission_status: 'ready' | 'requires_eviction' | 'blocked'
}

export type IdorisCapacity =
  | { state: 'observed'; entries: IdorisCapacityEntry[] }
  | { state: 'unavailable' }
  | { state: 'error' }

export interface IdorisStatusResponse {
  status: 'ok'
  service: 'idoris'
  version: string
  contract_version: string
  instance_id: string
  components: number
  runtimes: number
  subscriptions: number
  budget_configured: boolean
  audit_configured: boolean
  capacity: IdorisCapacity
}

export type IdorisUnavailableReason =
  | 'handoff-missing'
  | 'handoff-invalid'
  | 'invalid-config'
  | 'unreachable'
  | 'http-error'
  | 'invalid-response'

export type IdorisStatusResult =
  | { state: 'available'; status: IdorisStatusResponse }
  | { state: 'unavailable'; reason: IdorisUnavailableReason }

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

/** `model.call` WS event payload (agent24-protocol's `ModelCallPayload`,
 * `rust/crates/agent24-protocol/src/events.rs`) — one completed
 * `_a24/model/complete` call. Kernel-broadcast, NOT wrapped in the `module`
 * envelope like AgentEar's own events (it's a first-party event type, not a
 * module-namespaced one) — see main/agentear-events.ts's `parseModelCallFrame`.
 * Deliberately carries no prompt/response content. */
export interface ModelCallEnvelope {
  module: string
  model_id: string | null
  tier: string | null
  served_by: string | null
  ok: boolean
  latency_ms: number
  prompt_tokens: number | null
  completion_tokens: number | null
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
