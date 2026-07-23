// Preload script — runs in the renderer's process before any renderer code,
// with access to Node APIs. Exposes a tightly scoped bridge to renderer via
// contextBridge. Capability modules will extend this surface in M1+.

import { contextBridge, ipcRenderer } from 'electron'
import {
  IpcChannels,
  type BackendProxyRequest,
  type BackendProxyResponse,
  type LlmStatusResult,
  type ModuleInfo,
  type ModuleInstallResult,
  type ModuleUninstallResult,
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
} as const

contextBridge.exposeInMainWorld('agent24', api)

export type Agent24API = typeof api
