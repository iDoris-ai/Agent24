// Shared IPC channel names and payload types between main and renderer.
// Capability modules will extend this in M1+.

export const IpcChannels = {
  NavPrefsGet: 'nav-prefs:get',
  NavPrefsSet: 'nav-prefs:set',
  CliToolOpenSettings: 'cli-tool:open-settings',
  CliToolStatus: 'cli-tool:status',
  CliToolInstall: 'cli-tool:install',
  CliToolRepairUser: 'cli-tool:repair-user',
  AppLocale: 'app:locale',
  AppPing: 'app:ping',
  AppVersion: 'app:version',
  AppSetLocale: 'app:set-locale',
  ShellOpenExternal: 'shell:open-external',
  BackendProxy: 'backend:proxy',
  BackendEndpoint: 'backend:endpoint',
  // A1: elevated daemon operations the generic `backendProxy` no longer
  // forwards. Each asks the user through a native dialog in the main process
  // before sending anything.
  ApprovalDecide: 'approval:decide',
  OsModuleSetEnabled: 'os-module:set-enabled',
  CommInstall: 'comm:install',
  // Private-key backup: native confirm + native save dialog in main; the
  // backend writes the file itself, so the key never crosses IPC.
  CommIdentityExport: 'comm:identity-export',
  // Show the file from the last successful export in the file manager.
  // The renderer may pass only that exact path; main ignores every other one.
  CommRevealExport: 'comm:reveal-export',
  // Elevated comm writes that used to ride the generic proxy. Strictly
  // validated in main; presence binding is pending CORE-7c/CORE-2d.
  CommContactAdd: 'comm:contact-add',
  CommSend: 'comm:send',
  // Restore from an encrypted backup: main picks the file (native open dialog)
  // and collects the password (its own window); the page only names the identity.
  CommImportPick: 'comm:import-pick',
  CommIdentityImport: 'comm:identity-import',
  // Whole-HOME import, relay list and unlock: elevated, so no longer on the
  // generic proxy. Main picks the folder, validates the list and collects the
  // password in its own window; the page supplies none of those.
  CommHomeImport: 'comm:home-import',
  CommRelaySet: 'comm:relay-set',
  CommUnlock: 'comm:unlock',
  // A-3: disable/enable or unpair AgentEar (PATCH / DELETE
  // /api/v1/attached/agentear). The module name is fixed in main; the
  // renderer cannot name another one. Same native-confirm rule as above.
  AgentEarSetEnabled: 'agentear:set-enabled',
  AgentEarUnpair: 'agentear:unpair',
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
  // SD-01: pauses (not cancels — the on-disk prefix is kept) the current
  // in-flight download. Resuming reuses OpenDesignComponentInstall itself.
  OpenDesignComponentPause: 'open-design-component:pause',
  // Push channel only (main -> renderer), mirrors AgentEarEvent's pattern.
  OpenDesignComponentProgress: 'open-design-component:progress',
  OmlxDetect: 'omlx:detect',
  OmlxModels: 'omlx:models',
  OmlxStart: 'omlx:start',
  OmlxStop: 'omlx:stop',
  OmlxWarmup: 'omlx:warmup',
  LlmStatus: 'llm:status',
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
  // ADR-DOC-01 D5: the Documenting OS's typed channel. The renderer names an
  // operation; main maps it to a fixed route (main/documents.ts).
  DocumentsRequest: 'documents:request',
  // Picks a file in a native dialog and imports it (main/documents-import.ts);
  // progress is pushed on DocumentsImportProgress.
  DocumentsImportFile: 'documents:import-file',
  DocumentsImportProgress: 'documents:import-progress',
  // The OS's job and document events, pushed (ADR-DOC-02 §7; main/documents.ts).
  DocumentsEvent: 'documents:event',
  // Knowledge settings (main/knowledge-settings.ts): the WeKnora endpoint, the
  // knowledge bases and the On/Off switch. The key goes in, never comes back.
  KnowledgeStatus: 'knowledge:status',
  KnowledgeConfigure: 'knowledge:configure',
  KnowledgeSetEnabled: 'knowledge:set-enabled',
} as const

/** What the renderer sees of the Knowledge settings: never the key. */
export interface KnowledgeStatus {
  enabled: boolean
  endpoint: string
  knowledgeBaseIds: string[]
  hasKey: boolean
  /** On, complete and activated in the daemon. */
  active: boolean
}

/** The key is optional: left out, the saved one is kept. */
export interface KnowledgeConfigureInput {
  endpoint: string
  knowledgeBaseIds: string[]
  apiKey?: string
}

export type KnowledgeSettingsResult = { ok: true } | { ok: false; reason: 'invalid' | 'not_saved' | 'unavailable' }

export type IpcChannel = typeof IpcChannels[keyof typeof IpcChannels]

export type HttpMethod = 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE'

export interface BackendProxyRequest {
  method: HttpMethod
  path: string
  body?: unknown
  /** One constrained header; authentication headers remain main-process owned. */
  idempotencyKey?: string
}

/** Result of the native "choose a backup file" step. Main reads and validates
 *  the file at once; the page learns only the file NAME (to suggest a
 *  nickname) and an opaque single-use ticket. The path and the contents stay in
 *  the main process. `cancelled: false` means the file was not a usable backup. */
export type CommImportPickResult =
  | { ok: true; ticket: string; fileName: string }
  | { ok: false; cancelled: true }
  | { ok: false; cancelled: false; message: string }

export interface BackendProxyResponse {
  ok: boolean
  status: number
  data: unknown
}

/** A1: a decision for `POST /api/v1/approvals/{id}`. The daemon offers the valid
 *  set per approval (`available_decisions`), so `type` is an open string here. */
export interface ApprovalDecisionInput {
  type: string
  reason?: string
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
  /** Set instead of an error when this build neither bakes a downloadable
   * Open Design component manifest nor finds a local runtime: the optional
   * component is simply not available, which is a state, not a failure. */
  notInstalled?: boolean
  size?: number
}

/** Mirrors main/open-design-component.ts's `OpenDesignComponentStatus` —
 * duplicated here (not imported) because this file is the main/renderer
 * boundary and must stay free of main-process-only modules. */
export interface OpenDesignComponentStatusResult {
  state: 'not-installed' | 'downloading' | 'paused' | 'verifying' | 'installed' | 'failed' | 'unavailable'
  received?: number
  total?: number
  size?: number
  dir?: string
  error?: string
  reason?: string
  /** SD-01: rolling-window transfer rate while `downloading`, bytes/sec; `0`
   * (not absent) means "waiting for network". */
  bytesPerSecond?: number
  /** SD-01: estimated seconds remaining; `null` while the rate is unknown
   * or zero. */
  etaSeconds?: number | null
  /** Present when the main process rewrote the download onto a loopback test origin. */
  overrideActive?: boolean
}

export interface OmlxDetectResult {
  url: string
  apiKey: string
  models: string[]
}

/** A server answered on this address but demands an API key (401/403). The
 * renderer should point the user at the API key field, not say "disconnected". */
export interface OmlxAuthRequiredResult {
  authRequired: true
  url: string
}

export interface OmlxModelsResult {
  ok: boolean
  models: string[]
  error?: string
  /** The server answered 401/403: it is running but needs an API key. */
  authRequired?: boolean
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

export interface LlmStatusResult {
  provider: 'omlx' | 'ollama' | 'none'
  url: string
  model: string
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

// ---- Documenting OS (ADR-DOC-01 D5) ----
// Parameters and bodies come from the generated api-client, so a contract
// change shows up here as a type error.

type DocSchemas = import('@agent24/api-client').components['schemas']
type DocOps = import('@agent24/api-client').operations
type ListQuery = NonNullable<DocOps['documentsListDocuments']['parameters']['query']>
type TextQuery = NonNullable<DocOps['documentsReadText']['parameters']['query']>
type FindBody = DocSchemas['DocumentsFindRequest']
type ExtractBody = DocSchemas['DocumentsExtractRequest']

/** The operations the page may ask for (ADR-DOC-02 §4). */
export type DocumentsRequest =
  | { op: 'capabilities' }
  | { op: 'list'; cursor?: ListQuery['cursor']; limit?: ListQuery['limit'] }
  | { op: 'get'; documentId: DocSchemas['DocumentIdString'] }
  | {
      op: 'readText'
      documentId: DocSchemas['DocumentIdString']
      revision: number
      block?: TextQuery['block']
      cursor?: string
      limit?: TextQuery['limit']
      requireComplete?: TextQuery['require_complete']
    }
  | {
      op: 'find'
      documentId: DocSchemas['DocumentIdString']
      revision: FindBody['revision']
      query: FindBody['query']
      cursor?: FindBody['cursor']
      limit?: FindBody['limit']
      requireComplete?: FindBody['require_complete']
    }
  | {
      op: 'extract'
      documentId: DocSchemas['DocumentIdString']
      revision: ExtractBody['revision']
      schema: ExtractBody['schema']
      rerun?: ExtractBody['rerun']
      /** Required with `rerun: true` (the `Idempotency-Key` header). */
      idempotencyKey?: string
    }
  | { op: 'getExtraction'; extractionId: DocSchemas['ExtractionIdString']; cursor?: string; limit?: number }
  | {
      op: 'renderPage'
      documentId: DocSchemas['DocumentIdString']
      revision: number
      page: number
      scale?: number
      region?: string
    }
  | { op: 'job'; jobId: DocSchemas['JobIdString'] }
  | { op: 'cancelJob'; jobId: DocSchemas['JobIdString'] }
  | { op: 'retryJob'; jobId: DocSchemas['JobIdString'] }

/** What each operation returns on success. */
export interface DocumentsResults {
  capabilities: DocSchemas['DocumentsCapabilities']
  list: DocSchemas['DocumentList']
  get: DocSchemas['DocumentDetail']
  readText: DocSchemas['DocumentsTextPage']
  find: DocSchemas['DocumentsFindResult']
  extract: DocSchemas['DocumentsJob']
  getExtraction: DocSchemas['DocumentsExtraction']
  /** The PNG in standard base64; `scale` is the one the OS used. */
  renderPage: { media_type: 'image/png'; bytes: number; scale: number; png: string }
  job: DocSchemas['DocumentsJob']
  cancelJob: DocSchemas['DocumentsJob']
  retryJob: DocSchemas['DocumentsJob']
}

/** The error envelope: the OS's or the kernel's, or one main makes for a
 * transport failure (`backend_not_ready`, `backend_unreachable`,
 * `backend_timeout` — outcome unknown, never retried here —
 * `invalid_response`, `invalid_request`). */
export type DocumentsError = DocSchemas['ErrorBody']

export type DocumentsResponse<Op extends DocumentsRequest['op'] = DocumentsRequest['op']> =
  | { ok: true; status: number; data: DocumentsResults[Op] }
  | { ok: false; status: number; error: DocumentsError }

/** An event from the Documenting OS (ADR-DOC-02 §7): a hint to read the job
 * or the list again, never the truth itself. */
export type DocumentsEvent =
  | { kind: 'job.progress'; payload: DocSchemas['DocumentsJobProgressEvent'] }
  | { kind: 'job.finished'; payload: DocSchemas['DocumentsJobFinishedEvent'] }
  | { kind: 'document.imported'; payload: DocSchemas['DocumentsDocumentImportedEvent'] }
  | { kind: 'revision.committed'; payload: DocSchemas['DocumentsRevisionCommittedEvent'] }

/** Bytes of the picked file sent so far, pushed while it uploads. */
export interface DocumentsImportProgress {
  sent: number
  total: number
}

export type CliToolPathStatus = { path: string } & (
  | { link: 'missing'; state: 'missing'; version: null }
  | { link: 'current'; state: 'current'; version: string }
  | { link: 'other'; state: 'outdated' | 'current' | 'newer'; version: string | null }
)

export interface CliToolStatus {
  state: 'missing' | 'outdated' | 'current' | 'newer' | 'unknown' | 'unavailable'
  path: string | null
  version: string | null
  bundledVersion: string | null
  paths: CliToolPathStatus[]
}

export type CliInstallResult =
  | { state: 'installed'; backup: string | null }
  | { state: 'cancelled' | 'failed' | 'move-app' }
