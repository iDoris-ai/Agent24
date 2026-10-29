// Preload script — runs in the renderer's process before any renderer code,
// with access to Node APIs. Exposes a tightly scoped bridge to renderer via
// contextBridge. Capability modules will extend this surface in M1+.

import { contextBridge, ipcRenderer } from 'electron'
import type { NavPrefs } from '../renderer/capabilities/types'
import {
  IpcChannels,
  type BackendEndpointResult,
  type CliToolStatus,
  type CliInstallResult,
  type DocumentsEvent,
  type DocumentsImportProgress,
  type DocumentsRequest,
  type DocumentsResponse,
  type BackendProxyRequest,
  type BackendProxyResponse,
  type ApprovalDecisionInput,
  type CreativeViewBounds,
  type CreativeViewResult,
  type LlmStatusResult,
  type ModelCallEnvelope,
  type OpenDesignComponentStatusResult,
  type OmlxAuthRequiredResult,
  type OmlxDetectResult,
  type OmlxModelsResult,
  type OmlxStartResult,
  type OmlxStopResult,
  type OmlxWarmupResult,
} from '../shared/ipc-types'

const api = {
  getNavPrefs: (): Promise<NavPrefs> => ipcRenderer.invoke(IpcChannels.NavPrefsGet),
  setNavPrefs: (prefs: NavPrefs): Promise<void> => ipcRenderer.invoke(IpcChannels.NavPrefsSet, prefs),
  onCliToolSettings: (cb: () => void): (() => void) => {
    ipcRenderer.on(IpcChannels.CliToolOpenSettings, cb)
    return () => ipcRenderer.removeListener(IpcChannels.CliToolOpenSettings, cb)
  },
  cliToolStatus: (): Promise<CliToolStatus> => ipcRenderer.invoke(IpcChannels.CliToolStatus),
  installCliTool: (): Promise<CliInstallResult> => ipcRenderer.invoke(IpcChannels.CliToolInstall),
  setAppLocale: (locale: import('../shared/i18n').Locale): Promise<void> => ipcRenderer.invoke(IpcChannels.AppLocale, locale),
  ping: (): Promise<string> => ipcRenderer.invoke(IpcChannels.AppPing),
  setUiLocale: (locale: string): Promise<void> => ipcRenderer.invoke(IpcChannels.AppSetLocale, locale),
  getAppVersion: (): Promise<string> => ipcRenderer.invoke(IpcChannels.AppVersion),
  openExternal: (url: string): Promise<void> => ipcRenderer.invoke(IpcChannels.ShellOpenExternal, url),
  backendProxy: (req: BackendProxyRequest): Promise<BackendProxyResponse> =>
    ipcRenderer.invoke(IpcChannels.BackendProxy, req),
  /** A1: elevated; main asks the user in a native dialog before sending. */
  approvalDecide: (id: string, decision: ApprovalDecisionInput): Promise<BackendProxyResponse> =>
    ipcRenderer.invoke(IpcChannels.ApprovalDecide, id, decision),
  /** A1: elevated; main asks the user in a native dialog before sending. */
  osModuleSetEnabled: (name: string, enabled: boolean): Promise<BackendProxyResponse> =>
    ipcRenderer.invoke(IpcChannels.OsModuleSetEnabled, name, enabled),
  backendEndpoint: (): Promise<BackendEndpointResult | null> =>
    ipcRenderer.invoke(IpcChannels.BackendEndpoint),
  creativeShow: (bounds: CreativeViewBounds): Promise<CreativeViewResult> =>
    ipcRenderer.invoke(IpcChannels.CreativeShow, bounds),
  creativeRestart: (bounds: CreativeViewBounds): Promise<CreativeViewResult> =>
    ipcRenderer.invoke(IpcChannels.CreativeRestart, bounds),
  creativeBounds: (bounds: CreativeViewBounds): Promise<void> =>
    ipcRenderer.invoke(IpcChannels.CreativeBounds, bounds),
  creativeHide: (): Promise<void> =>
    ipcRenderer.invoke(IpcChannels.CreativeHide),
  openDesignComponentStatus: (): Promise<OpenDesignComponentStatusResult> =>
    ipcRenderer.invoke(IpcChannels.OpenDesignComponentStatus),
  openDesignComponentInstall: (): Promise<OpenDesignComponentStatusResult> =>
    ipcRenderer.invoke(IpcChannels.OpenDesignComponentInstall),
  openDesignComponentPause: (): Promise<OpenDesignComponentStatusResult> =>
    ipcRenderer.invoke(IpcChannels.OpenDesignComponentPause),
  onOpenDesignComponentProgress: (cb: (status: OpenDesignComponentStatusResult) => void): (() => void) => {
    const listener = (_event: unknown, status: OpenDesignComponentStatusResult): void => cb(status)
    ipcRenderer.on(IpcChannels.OpenDesignComponentProgress, listener)
    return () => ipcRenderer.removeListener(IpcChannels.OpenDesignComponentProgress, listener)
  },
  omlxDetect: (): Promise<OmlxDetectResult | OmlxAuthRequiredResult | null> =>
    ipcRenderer.invoke(IpcChannels.OmlxDetect),
  omlxModels: (url: string, apiKey: string): Promise<OmlxModelsResult> =>
    ipcRenderer.invoke(IpcChannels.OmlxModels, url, apiKey),
  omlxStart: (port: number, apiKey: string): Promise<OmlxStartResult> =>
    ipcRenderer.invoke(IpcChannels.OmlxStart, port, apiKey),
  omlxStop: (): Promise<OmlxStopResult> =>
    ipcRenderer.invoke(IpcChannels.OmlxStop),
  omlxWarmup: (url: string, apiKey: string, modelId: string): Promise<OmlxWarmupResult> =>
    ipcRenderer.invoke(IpcChannels.OmlxWarmup, url, apiKey, modelId),
  llmStatus: (): Promise<LlmStatusResult> =>
    ipcRenderer.invoke(IpcChannels.LlmStatus),
  // A3-4: subscribe to agentear.event/1 envelopes pushed from main
  // (agentear-events.ts / agentear-log.ts). Returns an unsubscribe function
  // so React effects can clean up on unmount without leaking listeners
  // across page switches.
  onAgentEarEvent: (cb: (envelope: unknown) => void): (() => void) => {
    const listener = (_event: unknown, envelope: unknown): void => cb(envelope)
    ipcRenderer.on(IpcChannels.AgentEarEvent, listener)
    return () => ipcRenderer.removeListener(IpcChannels.AgentEarEvent, listener)
  },
  // A3-4 review M5: pull the main process's current log (agentear-log.ts) on
  // mount, so the panel doesn't start empty every time it's navigated back to.
  agentearSnapshot: (): Promise<unknown[]> =>
    ipcRenderer.invoke(IpcChannels.AgentEarSnapshot),
  // ME4-desktop-model-ui: same push/pull pair, for the kernel's `model.call`
  // WS events (main/model-call-log.ts) — mirrors onAgentEarEvent/agentearSnapshot.
  onModelCallEvent: (cb: (call: ModelCallEnvelope) => void): (() => void) => {
    const listener = (_event: unknown, call: ModelCallEnvelope): void => cb(call)
    ipcRenderer.on(IpcChannels.ModelCallEvent, listener)
    return () => ipcRenderer.removeListener(IpcChannels.ModelCallEvent, listener)
  },
  modelCallSnapshot: (): Promise<ModelCallEnvelope[]> =>
    ipcRenderer.invoke(IpcChannels.ModelCallSnapshot),
  // ADR-DOC-01 D5: the Documenting OS, by operation, never by path.
  documents: {
    request: <R extends DocumentsRequest>(req: R): Promise<DocumentsResponse<R['op']>> =>
      ipcRenderer.invoke(IpcChannels.DocumentsRequest, req),
    /** Picks a PDF, JPEG or PNG and imports it; null if the user cancels. */
    importFile: (): Promise<DocumentsResponse<'job'> | null> =>
      ipcRenderer.invoke(IpcChannels.DocumentsImportFile),
    onImportProgress: (cb: (p: DocumentsImportProgress) => void): (() => void) => {
      const listener = (_event: unknown, p: DocumentsImportProgress): void => cb(p)
      ipcRenderer.on(IpcChannels.DocumentsImportProgress, listener)
      return () => ipcRenderer.removeListener(IpcChannels.DocumentsImportProgress, listener)
    },
    /** The OS's job and document events (hints: read the job or list again). */
    onEvent: (cb: (e: DocumentsEvent) => void): (() => void) => {
      const listener = (_event: unknown, e: DocumentsEvent): void => cb(e)
      ipcRenderer.on(IpcChannels.DocumentsEvent, listener)
      return () => ipcRenderer.removeListener(IpcChannels.DocumentsEvent, listener)
    },
  },
} as const

contextBridge.exposeInMainWorld('agent24', api)

export type Agent24API = typeof api
