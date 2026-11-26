// Shared IPC channel names and payload types between main and renderer.
// Capability modules will extend this in M1+.

export const IpcChannels = {
  NavPrefsGet: 'nav-prefs:get',
  NavPrefsSet: 'nav-prefs:set',
  // UI-02: the main chat's history on this machine (main/chat-history.ts).
  ChatHistoryList: 'chat-history:list',
  ChatHistoryGet: 'chat-history:get',
  ChatHistorySave: 'chat-history:save',
  ChatHistoryDelete: 'chat-history:delete',
  ChatHistoryClear: 'chat-history:clear',
  ChatHistorySetTitle: 'chat-history:set-title',
  // UI-06: groups and archiving in the history list.
  ChatHistoryOrganize: 'chat-history:organize',
  // M4 A2: a run of the conversation becomes a task; main's own changes to
  // runs and waiting items, pushed to the window.
  ChatHistoryPromoteRun: 'chat-history:promote-run',
  ChatHistoryChanged: 'chat-history:changed',
  // M6 C1: the user opened a conversation (or has it open): nothing of it
  // is unseen any more (docs/ui/ASSISTANT-C.md §4).
  ChatHistoryMarkSeen: 'chat-history:mark-seen',
  // M4 A3: a run's or an approval's change, from the daemon's /events.
  RunEvent: 'run:event',
  // M4 A6: the conversation a schedule's runs go to (A5), for its page.
  ScheduleConversation: 'schedule:conversation',
  ChatGroupsGet: 'chat-groups:get',
  ChatGroupsCreate: 'chat-groups:create',
  ChatGroupsRename: 'chat-groups:rename',
  ChatGroupsDelete: 'chat-groups:delete',
  ChatGroupsSetCollapsed: 'chat-groups:set-collapsed',
  CliToolOpenSettings: 'cli-tool:open-settings',
  CliToolStatus: 'cli-tool:status',
  CliToolInstall: 'cli-tool:install',
  CliToolRepairUser: 'cli-tool:repair-user',
  // M5 B4c2: Settings → General → background service switch.
  ResidentServiceStatus: 'resident-service:status',
  ResidentServiceSet: 'resident-service:set',
  AppLocale: 'app:locale',
  AppPing: 'app:ping',
  AppVersion: 'app:version',
  AppSetLocale: 'app:set-locale',
  ShellOpenExternal: 'shell:open-external',
  BackendProxy: 'backend:proxy',
  BackendEndpoint: 'backend:endpoint',
  // QA 0.6.1 §3c: why the service daemon did not take this app, if it did not.
  BackendRefusal: 'backend:refusal',
  // A1: elevated daemon operations the generic `backendProxy` no longer
  // forwards. Each asks the user through a native dialog in the main process
  // before sending anything.
  ApprovalDecide: 'approval:decide',
  OsModuleSetEnabled: 'os-module:set-enabled',
  // T1-2: uninstall the documents module / install the one the app carries.
  // No arguments: the module is fixed in main. Each runs the app's CLI.
  OsModuleUninstall: 'os-module:uninstall',
  OsModuleInstallBundled: 'os-module:install-bundled',
  // Sin90 writes. Main attaches the human key; the renderer names an operation.
  Sin90Write: 'sin90:write',
  Sin90KeyStatus: 'sin90:key-status',
  ClipboardWriteText: 'clipboard:write-text',
  Sin90ExportToFile: 'sin90:export-to-file',
  Sin90ImportFromFile: 'sin90:import-from-file',
  // Pushed from the one /api/v1/events socket. `{kind:'connected'}` is a reconnect.
  Sin90Event: 'sin90:event',
  // OSI-1: download a registered domain OS (Sin90) and `agent24 os install` it.
  OsPackageInstall: 'os-package:install',
  OsPackageLatest: 'os-package:latest',
  OsPackageProgress: 'os-package:progress',
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
  CommContactRename: 'comm:contact-rename',
  CommContactRemove: 'comm:contact-remove',
  CommSend: 'comm:send',
  CommGroupCreate: 'comm:group-create',
  CommGroupSend: 'comm:group-send',
  CommGroupAccept: 'comm:group-accept',
  CommGroupRetry: 'comm:group-retry',
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
  // Skill loader SL-4: `POST /api/v1/skills/{id}/activate` is Elevated and not
  // on the generic proxy. Main reads the daemon's own record of the
  // revisions, shows its own summary natively, then posts once.
  SkillsActivate: 'skills:activate',
  SkillsRollback: 'skills:rollback',
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
  OmlxStart: 'omlx:start',
  OmlxStop: 'omlx:stop',
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
  // T1-1: what this launch did with the documents module the app carries.
  DocumentsModuleStatus: 'documents:module-status',
  // D1: the LibreOffice build Office → PDF converts with (main/office-engine.ts).
  DocumentsOfficeEngineStatus: 'documents:office-engine-status',
  DocumentsOfficeEngineInstall: 'documents:office-engine-install',
  DocumentsOfficeEngineChanged: 'documents:office-engine-changed',
  // D2b: grant the Agent documents.list / get / compose for 30 days.
  DocumentsAgentAccess: 'documents:agent-access',
  // D4: translated HTML → a DOCX in Downloads and a PDF in the document library.
  DocumentsExportTranslation: 'documents:export-translation',
  /** D4: show an exported file of the Downloads folder in Finder (its name only). */
  DocumentsRevealDownload: 'documents:reveal-download',
  /** D5a: a library document's file saved to Downloads (its id, its title). */
  DocumentsSaveDownload: 'documents:save-download',
  /** L8 DOC-4: a clean copy saved where the user picks, then its receipt. */
  DocumentsDeliver: 'documents:deliver',
  // Knowledge settings (main/knowledge-settings.ts): the WeKnora endpoint, the
  // knowledge bases and the On/Off switch. The key goes in, never comes back.
  KnowledgeStatus: 'knowledge:status',
  /** K2: check the Knowledge server now; answers the status with the result. */
  KnowledgeCheck: 'knowledge:check',
  KnowledgeConfigure: 'knowledge:configure',
  KnowledgeSetEnabled: 'knowledge:set-enabled',
  /** L7-K2: whether mock onboarding can start, and whether the server answers. */
  KnowledgeOnboardPreflight: 'knowledge:onboard-preflight',
  /** L7-K2: run mock onboarding. The activation code and the key never come back. */
  KnowledgeOnboardRun: 'knowledge:onboard-run',
  /** L7-K2 A3: pick files in main and upload them. Paths never come back. */
  KnowledgeUploadPick: 'knowledge:upload-pick',
  KnowledgeUploadList: 'knowledge:upload-list',
  /** Arg is the uploadId, a string of at most 64 characters. */
  KnowledgeUploadCancel: 'knowledge:upload-cancel',
  /** Main → renderer. The full upload list. No path, bytes, credential or URL. */
  KnowledgeUploadChanged: 'knowledge:upload-changed',
  // KB-C2-03 (main/knowledge-view.ts): the KnowledgeView container. The page
  // sends only where to draw it; the address, partition and policy are main's.
  KnowledgeViewShow: 'knowledge-view:show',
  KnowledgeViewBounds: 'knowledge-view:bounds',
  KnowledgeViewHide: 'knowledge-view:hide',
  KnowledgeViewReload: 'knowledge-view:reload',
  KnowledgeViewState: 'knowledge-view:state',
  // KB-C2-04b-1 (U7.2): one citation's plain text, or null to clear the strip.
  KnowledgeViewCitation: 'knowledge-view:citation',
  // KB-C2-04b-2 (U7.2): the confirmation dialog. null closes it.
  KnowledgeViewConfirm: 'knowledge-view:confirm',
  KnowledgeViewAnswer: 'knowledge-view:answer',
  // M4 A7: the renderer put a confirm dialog up, and which conversation was open.
  KnowledgeViewShown: 'knowledge-view:shown',
  // The conversation's draft attachments. Changed is the full list, main → renderer.
  KnowledgeDraftChanged: 'knowledge-draft:changed',
  KnowledgeDraftList: 'knowledge-draft:list',
  KnowledgeDraftRemove: 'knowledge-draft:remove',
  // KB-SOP-01a (#1669): may this conversation become a knowledge entry (main asks the user).
  KnowledgeEntryAdmit: 'knowledge-entry:admit',
  // KB-SOP-01b-2 (#1669): local knowledge entry drafts (main/knowledge-entry-ipc.ts).
  KnowledgeEntryDraftCreate: 'knowledge-entry:draft-create',
  KnowledgeEntryDraftList: 'knowledge-entry:draft-list',
  KnowledgeEntryDraftGet: 'knowledge-entry:draft-get',
  KnowledgeEntryDraftUpdate: 'knowledge-entry:draft-update',
  KnowledgeEntryDraftRedact: 'knowledge-entry:draft-redact',
  KnowledgeEntryDraftWithdraw: 'knowledge-entry:draft-withdraw',
  // KB-SOP-01b (#1670): submit for review, read it back, withdraw the submission (the local draft stays).
  KnowledgeEntrySubmit: 'knowledge-entry:submit',
  KnowledgeEntrySubmitStatus: 'knowledge-entry:submit-status',
  KnowledgeEntrySubmitRetract: 'knowledge-entry:submit-retract',
} as const

/** T1-1/T1-2: the documents module as it is on disk now. A daemon reads
 * installed modules only when it starts, so what it lists can lag behind. */
export interface DocumentsModuleStatus {
  /** The app carries a module it can install (macOS packaged builds). */
  bundled: boolean
  /** `<packages root>/documents` is there. */
  installed: boolean
  /** The running daemon was started outside the app: restart it with the CLI, not the tray. */
  externalDaemon: boolean
  /** os.json has documents turned off (as `GET /api/v1/os` reports it); absent when the daemon did not say.
   * Until the daemon restarts, a module turned off while running answers 503 `module_stopping`,
   * like a crashed one, not `module_disabled`. */
  turnedOff?: boolean
}

/** What the renderer sees of the Knowledge settings: never the key. */
export interface KnowledgeStatus {
  enabled: boolean
  endpoint: string
  knowledgeBaseIds: string[]
  hasKey: boolean
  /** On, complete and activated in the daemon. */
  active: boolean
  /** K2: whether the server answered the last check (absent: not checked by this main). */
  reachability?: { state: 'unknown' | 'ok' | 'unreachable' | 'unauthorized' | 'not_found'; checkedAt: string | null }
}

/** The key is optional: left out, the saved one is kept. */
export interface KnowledgeConfigureInput {
  endpoint: string
  knowledgeBaseIds: string[]
  apiKey?: string
}

/** L7-K2 preflight. No activation code, no key. */
export interface KnowledgeOnboardPreflight {
  /** A usable `knowledge-activation.json` is in userData. */
  activated: boolean
  /** The file is present but rejected (mode, shape, endpoint or code). */
  invalidActivation: boolean
  /** An endpoint is already saved; onboarding will not overwrite it. */
  alreadyBound: boolean
  /** `GET {endpoint}/health` was 2xx. Null when there is no usable activation. */
  reachable: boolean | null
  formats: readonly ['md', 'markdown', 'txt']
  maxBytes: number
}

/** L7-K2 result. A success names the knowledge base only — never the key or the code. */
export type KnowledgeOnboardResult =
  /** activeNow: the daemon took the binding now; false when it was not up and takes it later from the saved settings. */
  | { ok: true; outcome: 'created' | 'existing'; knowledgeBaseId: string; configMismatch: boolean; activeNow: boolean }
  | {
    ok: false
    code:
      | 'local_only_unavailable'
      | 'unavailable'
      | 'already_bound'
      | 'not_activated'
      | 'activation_invalid'
      | 'in_progress'
      | 'not_offered'
      | 'unreachable'
      | 'server_error'
      | 'not_saved'
  }

/** L7-K2 A3: one picked file, as the renderer may see it. No path, bytes, credential or URL. */
export interface KnowledgeUpload {
  uploadId: string
  fileName: string
  state: 'refused' | 'sending' | 'saved' | 'processing' | 'indexed' | 'failed' | 'cancelled' | 'unknown'
  reason: string | null
  jobId: string | null
  enrichment: string | null
  /** The server marked this cost final. */
  costFinal: boolean
  /** True only when the cost is final and every item is measured exact with units 0. */
  costZero: boolean
  /** True when any cost item is measured unknown. */
  costUnknown: boolean
  originalRetained: boolean
  updatedAt: number
}

export type {
  KnowledgeViewBounds,
  KnowledgeViewFailure,
  KnowledgeViewResult,
  KnowledgeViewState,
  KnowledgeViewTheme,
} from './knowledge-view-types'

/** Main → renderer. The three ids only: no revision fields (U7.2). null clears the strip. */
export type KnowledgeViewCitation = {
  title: string | null
  text: string
  ref: { knowledge_base_id: string; knowledge_id: string; chunk_id: string }
} | null

/** Main → renderer. null closes the dialog (U7.2). */
export type KnowledgeViewConfirmAsk = {
  id: string
  count: number
  titles: Array<string | null>
} | null

/** One draft attachment as the renderer sees it (U7.2). */
export type KnowledgeDraftEntry = {
  id: string
  title: string | null
  text: string
  ref: { knowledge_base_id: string; knowledge_id: string; chunk_id: string }
}

/** KB-SOP-01a: the answer to "沉淀为知识条目" (main/knowledge-entry-ipc.ts). */
export type KnowledgeEntryAdmission =
  | { ok: true }
  | { ok: false; reason: 'floor_missing' | 'restricted' | 'unknown' | 'too_many_runs' | 'not_confirmed' | 'not_found' | 'busy' | 'knowledge_off' }

/** KB-SOP-01b-2: one message of the admitted conversation, for choosing the range. */
export interface KnowledgeEntryMessagePreview {
  role: 'user' | 'assistant'
  /** The start of the message on one line. */
  preview: string
}

/** What the renderer gets for "沉淀为知识条目": on yes, a one-use ticket
 *  for the very record main judged, and its messages to choose a range from. */
export type KnowledgeEntryAdmitAnswer =
  | { ok: true; ticket: string; messages: KnowledgeEntryMessagePreview[] }
  | Extract<KnowledgeEntryAdmission, { ok: false }>

export type KnowledgeEntryKind = 'sop' | 'faq' | 'note'
export type KnowledgeEntryFinding = 'private_key' | 'credential' | 'email' | 'phone' | 'id_card' | 'bank_card' | 'path' | 'internal_address'
/** `same`: bound to what Knowledge is bound to now; `changed`: another target
 *  ("需要重新选目标库"); `none`: Knowledge is off or not bound now. */
export type KnowledgeEntryTarget = 'same' | 'changed' | 'none'

export interface KnowledgeEntryDraftSummary {
  id: string
  kind: KnowledgeEntryKind
  title: string
  updated_at: string
  target: KnowledgeEntryTarget
  /** The post-scan's categories; any one blocks submission. */
  findings: KnowledgeEntryFinding[]
}

export interface KnowledgeEntryDraftView extends KnowledgeEntryDraftSummary {
  body: string
  conversation_id: string
  message_from: number
  message_to: number
  created_at: string
  content_sha256: string
  /** The first restricted-Markdown problem (CONTRACT-OPS-v2 W8.2), judged now; any one blocks submission. */
  format: KnowledgeEntryFormatProblem | null
  /** KB-SOP-01b: this draft's submission, if it has been sent. */
  submission: KnowledgeEntrySubmission
  /** The knowledge bases the draft can be submitted to (the binding in effect). */
  knowledge_base_ids: string[]
}

/** One of `knowledge-entry-markdown.ts`'s problem kinds, and its 1-based line. */
export interface KnowledgeEntryFormatProblem {
  kind: string
  line: number
}

export type KnowledgeEntryCandidateStatus = 'submitted' | 'approved' | 'returned' | 'withdrawn'

/** What the renderer learns of a submission: never the key, the request body or session_ref. */
export interface KnowledgeEntrySubmission {
  /** Sent, outcome not known yet (W1.5). */
  pending: boolean
  /** Pending and not to be resent (the binding or the credential changed, too long ago, or the record cannot be read): the user has to check. */
  stuck: boolean
  candidate: {
    status: KnowledgeEntryCandidateStatus
    target_knowledge_base_id: string
    return_reason: string | null
    note: string | null
    submitted_at: string
    checked_at: string
    /** The local draft was edited after it was submitted. */
    edited: boolean
  } | null
}

export type KnowledgeEntrySubmitRefusal =
  | 'not_found' | 'target_changed' | 'invalid_target' | 'findings' | 'unsupported_format'
  | 'already_submitted' | 'already_approved' | 'busy' | 'local_storage' | 'knowledge_off' | 'outcome_unknown'

export type KnowledgeEntrySubmitAnswer =
  | { ok: true; submission: KnowledgeEntrySubmission }
  | { ok: false; reason: KnowledgeEntrySubmitRefusal; problem?: KnowledgeEntryFormatProblem; submission?: KnowledgeEntrySubmission }
  /** The server's answer, or a local one standing for it (`not_ready`, `cancelled`). `unknown`: it may have been carried out (W1.5). */
  | { ok: false; reason: 'server'; code: string; serverReason?: string; unknown: boolean; submission: KnowledgeEntrySubmission }

/** How the model step went; anything but `drafted` gave the empty template. */
export type KnowledgeEntryModelOutcome = 'drafted' | 'no_model' | 'needs_confirmation' | 'busy' | 'failed' | 'unusable' | 'held_back'

export type KnowledgeEntryDraftCreated =
  | { ok: true; draft: KnowledgeEntryDraftView; model: KnowledgeEntryModelOutcome; hints: string[] }
  | { ok: false; reason: 'ticket_expired' | 'invalid_range' | 'too_long' | 'too_many' | 'knowledge_off' | 'busy' | 'not_saved' }

export type KnowledgeEntryDraftSaved =
  | { ok: true; draft: KnowledgeEntryDraftView }
  /** `too_long`: redacting would make the body longer than 64 KiB (placeholders are longer than some hits). */
  | { ok: false; reason: 'not_found' | 'invalid' | 'not_saved' | 'too_long' }

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

/** OSI-1: main → renderer while `OsPackageInstall` is downloading. */
export interface OsPackageProgress {
  id: string
  phase: 'download' | 'verify' | 'extract' | 'install'
  received: number
  total: number | null
}

/** A1: a decision for `POST /api/v1/approvals/{id}`. The daemon offers the valid
 *  set per approval (`available_decisions`), so `type` is an open string here. */
export interface ApprovalDecisionInput {
  type: string
  reason?: string
}

/** Why the daemon this app spawned last failed to start. `error` is the
 * daemon's own text (the `failed` line, or the last stderr line). */
export interface DaemonStartupFailure {
  code: string
  error: string
}

/** FU-93: the backend daemon's actual (dynamic) port, so the sidebar can show
 * it instead of a hardcoded guess. `port` is null while the daemon has not
 * announced its ready line. `startupFailure` is the last start failure, when
 * there is one. */
export interface BackendEndpointResult {
  port: number | null
  startupFailure?: DaemonStartupFailure
}

/** The service daemon did not enrol this app: its code, and whether retrying
 * can help (`permanent`: identity_rejected, unsupported_version). */
export interface BackendRefusalResult {
  code: string
  permanent: boolean
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

export interface OmlxStartResult {
  ok: boolean
  url: string
  error?: string
}

export interface OmlxStopResult {
  ok: boolean
  /** False when this app had not spawned oMLX, so nothing was signalled. */
  stopped: boolean
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
  /** D2: a new document of pages of existing PDFs (split: one part; merge: several). */
  | { op: 'compose'; parts: Array<{ documentId: DocSchemas['DocumentIdString']; pages?: string }>; title?: string }
  /** D1: an Office document (DOCX/XLSX/PPTX) as a new PDF document. */
  | { op: 'convert'; documentId: DocSchemas['DocumentIdString']; title?: string }
  /** E1b: a DOCX's paragraphs, by index. */
  | { op: 'paragraphs'; documentId: DocSchemas['DocumentIdString'] }
  /** E1b: some paragraphs of a DOCX edited (plainly or as tracked changes) into a new document. */
  | {
      op: 'patch'
      documentId: DocSchemas['DocumentIdString']
      edits: Array<{ index: number; text: string; tracked?: boolean; expect?: string }>
      title?: string
      author?: string
    }
  /** D5a: a DOCX template's `{{fields}}`. */
  | { op: 'fields'; documentId: DocSchemas['DocumentIdString'] }
  /** L8 DOC-4: what a DOCX/PDF still carries before it is sent (read-only). */
  | { op: 'preflight'; documentId: DocSchemas['DocumentIdString'] }
  /** L8 DOC-4: a clean copy of the version checked (a new document), the
   * receipt once its file is saved, and the latest receipts. */
  | { op: 'deliver'; documentId: DocSchemas['DocumentIdString']; expectRevision: number }
  | { op: 'receipt'; documentId: DocSchemas['DocumentIdString']; revision: number; copyDocumentId: DocSchemas['DocumentIdString']; savedTo: string }
  | { op: 'receipts'; documentId?: DocSchemas['DocumentIdString'] }
  /** D5a: the template with its fields filled, as a new document. */
  | { op: 'fill'; documentId: DocSchemas['DocumentIdString']; values: Record<string, string>; title?: string }
  /** D5a: the head revision's file (base64), to be saved. */
  | { op: 'content'; documentId: DocSchemas['DocumentIdString']; revision?: number }
  | { op: 'cancelJob'; jobId: DocSchemas['JobIdString'] }
  | { op: 'retryJob'; jobId: DocSchemas['JobIdString'] }
  /** L6-F2: soft delete, restore within 7 days, and what can be restored. */
  | { op: 'delete'; documentId: DocSchemas['DocumentIdString'] }
  | { op: 'restore'; documentId: DocSchemas['DocumentIdString'] }
  | { op: 'recentlyDeleted' }
  /** L6-F5: a document's versions, and an old one made current again over
   * the version the user saw (`expectRevision`). */
  | { op: 'revisions'; documentId: DocSchemas['DocumentIdString'] }
  | { op: 'restoreRevision'; documentId: DocSchemas['DocumentIdString']; revision: number; expectRevision: number }
  /** L6-S05: two documents, or two versions of one, compared by their text
   * (no `revision` = the current version). */
  | { op: 'compare'; left: CompareTarget; right: CompareTarget; limit?: number; offset?: number }

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
  /** The import job of the new document. */
  compose: DocSchemas['DocumentsJob']
  /** The import job of the new PDF document. */
  convert: DocSchemas['DocumentsJob']
  paragraphs: { document_id: string; revision: number; paragraphs: Array<{ index: number; text: string; in_table: boolean }> }
  patch: DocSchemas['DocumentsJob']
  fields: { document_id: string; revision: number; fields: Array<{ name: string; count: number }> }
  fill: DocSchemas['DocumentsJob']
  preflight: DocumentPreflight
  deliver: DocSchemas['DocumentsJob']
  receipt: DeliveryReceipt
  receipts: { receipts: DeliveryReceipt[] }
  content: { media_type: string; bytes: number; data: string }
  cancelJob: DocSchemas['DocumentsJob']
  retryJob: DocSchemas['DocumentsJob']
  delete: DeletedDocument
  restore: DocSchemas['DocumentDetail']
  recentlyDeleted: { documents: DeletedDocument[] }
  revisions: DocumentVersions
  restoreRevision: SavedVersion
  compare: DocumentComparison
}

/** L8 DOC-4: agent24-documents `preflight.rs`; only kinds found are listed. */
export type PreflightKind =
  | 'tracked_insertions' | 'tracked_deletions' | 'comments' | 'unfilled_fields'
  | 'author' | 'last_modified_by' | 'hidden_text' | 'pdf_form' | 'pdf_annotations'
export interface DocumentPreflight {
  document_id: string
  revision: number
  title: string | null
  format: 'docx' | 'pdf'
  clean: boolean
  findings: Array<{ kind: PreflightKind; count: number; examples: string[] }>
  /** A DOCX (a clean copy can be made of it from the delivery card); a PDF is only reported. */
  cleanable: boolean
}

/** L8 DOC-4: agent24-documents `deliver.rs`; the hash and format are the
 * library copy's own. `again`: the same file was saved for it before. */
export interface DeliveryReceipt {
  document_id: string
  title: string | null
  revision: number
  at: string
  format: 'docx' | 'pdf'
  file_name: string
  saved_to: string
  sha256: string
  copy_document_id: string
  again: boolean
}

/** L8 DOC-4: `documents:deliver` (main `doc-deliver.ts`). */
export interface DeliverRequest {
  documentId: string
  /** The version the user checked (preflight's `revision`). */
  revision: number
  format: 'docx' | 'pdf'
  /** The source document's title, for the file name offered. */
  title: string
}
export type DeliverResult =
  | { ok: true; savedTo: string; fileName: string; receipt: DeliveryReceipt }
  | { ok: false; canceled: true }
  /** `savedTo` only when the file was written (stage `receipt`). */
  | { ok: false; canceled?: false; stage: 'copy' | 'convert' | 'write' | 'receipt'; code: string; message: string; savedTo?: string }

export interface CompareTarget { documentId: DocSchemas['DocumentIdString']; revision?: number }

/** L6-S05: where a change is in its document (agent24-documents `compare.rs`). */
export type CompareBlockRef = { paragraph_index: number } | { block_id: string; page: number }
export interface CompareSide { block_ref: CompareBlockRef; text: string }
export interface DocumentChange {
  id: string
  kind: 'added' | 'removed' | 'changed' | 'unaligned'
  left: CompareSide | null
  right: CompareSide | null
  spans: Array<{ op: 'equal' | 'insert' | 'delete'; text: string }>
}
/** `summary` counts every change; `changes` is one page of them. */
export interface DocumentComparison {
  left: { document_id: string; revision: number; media_type: string }
  right: { document_id: string; revision: number; media_type: string }
  identical: boolean
  only_text: boolean
  summary: { total: number; added: number; removed: number; changed: number; unaligned: number }
  changes: DocumentChange[]
  truncated: boolean
  next_offset: number | null
}

/** L6-F5: a document's versions, newest first (agent24-documents `versions.rs`). */
export interface DocumentVersions {
  document_id: string
  head_revision: number
  revisions: Array<{ revision: number; content_sha256: string; size: number; media_type: string; origin: 'import' | 'commit'; created_at: string }>
}

/** L6-F5: a version saved; `committed` false when nothing was written. */
export interface SavedVersion {
  document_id: string
  title: string
  revision: number
  previous_revision: number
  committed: boolean
}

/** L6-F2: a document deleted less than 7 days ago (agent24-documents `trash.rs`). */
export interface DeletedDocument {
  document_id: string
  title: string
  media_type: string
  created_at: string
  deleted_at: string
  restorable_until: string
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

/** D4: LibreOffice wrote the DOCX into Downloads, and the PDF's import finished. */
export interface TranslationExportResult {
  ok: boolean
  docxName?: string
  code?: string
  message?: string
}

/** D2b: whether the Agent may use the documents library (host grant, 30 days). */
export interface DocAgentAccess {
  enabled: boolean
  until: string | null
  ok?: boolean
  code?: string
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

/** M5 B4c2: whether this app can run the LaunchAgent (a packaged macOS app
 *  that ships its daemon) and whether its plist is there now. */
export interface ResidentServiceStatus { available: boolean; installed: boolean }

/** The switch's outcome; a failure names the step and the daemon's or the
 *  CLI's code (`install`/`start` when turning on, `shutdown`/`uninstall` off). */
export type ResidentServiceResult =
  | { ok: true }
  | { ok: false; stage: 'install' | 'start' | 'shutdown' | 'uninstall' | 'unavailable'; code: string }

/** M4 A3: the daemon events that change what a run is waiting on or where
 *  it stands (agent24-protocol `EventBody`). */
export const RUN_EVENT_TYPES = ['run.started', 'run.completed', 'run.failed', 'run.cancelled', 'approval.required', 'approval.resolved'] as const
/** M4 A3: one of them, as main forwards it: only which run and what happened. */
export interface RunEvent {
  type: (typeof RUN_EVENT_TYPES)[number]
  run_id: string
}

/** D1: the Office → PDF converter (LibreOffice) on this machine. */
export interface OfficeEngineStatus {
  state: 'missing' | 'installing' | 'ready' | 'failed'
  /** While installing: bytes downloaded so far, and the total when known. */
  received?: number
  total?: number
  /** When failed: why, shown as is. */
  error?: string
}
