// Preload script — runs in the renderer's process before any renderer code,
// with access to Node APIs. Exposes a tightly scoped bridge to renderer via
// contextBridge. Capability modules will extend this surface in M1+.

import { contextBridge, ipcRenderer } from 'electron'
import {
  IpcChannels,
  type BackendEndpointResult,
  type DocumentsImportProgress,
  type DocumentsRequest,
  type DocumentsResponse,
  type BackendProxyRequest,
  type BackendProxyResponse,
  type CreativeViewBounds,
  type CreativeViewResult,
  type DiscoverFilter,
  type DiscoveredModule,
  type LlmStatusResult,
  type IdorisStatusResult,
  type ModuleInfo,
  type ModuleInstallResult,
  type ModuleUninstallResult,
  type ModelCallEnvelope,
  type OpenDesignComponentStatusResult,
  type OmlxDetectResult,
  type OmlxModelsResult,
  type OmlxStartResult,
  type OmlxStopResult,
  type OmlxWarmupResult,
} from '../shared/ipc-types'

const api = {
  ping: (): Promise<string> => ipcRenderer.invoke(IpcChannels.AppPing),
  getAppVersion: (): Promise<string> => ipcRenderer.invoke(IpcChannels.AppVersion),
  openExternal: (url: string): Promise<void> => ipcRenderer.invoke(IpcChannels.ShellOpenExternal, url),
  backendProxy: (req: BackendProxyRequest): Promise<BackendProxyResponse> =>
    ipcRenderer.invoke(IpcChannels.BackendProxy, req),
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
  onOpenDesignComponentProgress: (cb: (status: OpenDesignComponentStatusResult) => void): (() => void) => {
    const listener = (_event: unknown, status: OpenDesignComponentStatusResult): void => cb(status)
    ipcRenderer.on(IpcChannels.OpenDesignComponentProgress, listener)
    return () => ipcRenderer.removeListener(IpcChannels.OpenDesignComponentProgress, listener)
  },
  omlxDetect: (): Promise<OmlxDetectResult | null> =>
    ipcRenderer.invoke(IpcChannels.OmlxDetect),
  omlxModels: (url: string, apiKey: string): Promise<OmlxModelsResult> =>
    ipcRenderer.invoke(IpcChannels.OmlxModels, url, apiKey),
  omlxStart: (port: number, apiKey: string): Promise<OmlxStartResult> =>
    ipcRenderer.invoke(IpcChannels.OmlxStart, port, apiKey),
  omlxStop: (): Promise<OmlxStopResult> =>
    ipcRenderer.invoke(IpcChannels.OmlxStop),
  omlxWarmup: (url: string, apiKey: string, modelId: string): Promise<OmlxWarmupResult> =>
    ipcRenderer.invoke(IpcChannels.OmlxWarmup, url, apiKey, modelId),
  modulesList: (): Promise<ModuleInfo[]> =>
    ipcRenderer.invoke(IpcChannels.ModulesList),
  modulesDiscover: (filter?: DiscoverFilter): Promise<DiscoveredModule[]> =>
    ipcRenderer.invoke(IpcChannels.ModulesDiscover, filter ?? {}),
  modulesEnable: (id: string): Promise<{ ok: boolean }> =>
    ipcRenderer.invoke(IpcChannels.ModulesEnable, id),
  modulesDisable: (id: string): Promise<{ ok: boolean }> =>
    ipcRenderer.invoke(IpcChannels.ModulesDisable, id),
  modulesInstall: (packageName: string): Promise<ModuleInstallResult> =>
    ipcRenderer.invoke(IpcChannels.ModulesInstall, packageName),
  modulesUninstall: (packageName: string, id?: string): Promise<ModuleUninstallResult> =>
    ipcRenderer.invoke(IpcChannels.ModulesUninstall, packageName, id),
  llmStatus: (): Promise<LlmStatusResult> =>
    ipcRenderer.invoke(IpcChannels.LlmStatus),
  idorisStatus: (): Promise<IdorisStatusResult> =>
    ipcRenderer.invoke(IpcChannels.IdorisStatus),
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
  },
} as const

contextBridge.exposeInMainWorld('agent24', api)

export type Agent24API = typeof api
