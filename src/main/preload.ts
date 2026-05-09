// Preload script — runs in the renderer's process before any renderer code,
// with access to Node APIs. Exposes a tightly scoped bridge to renderer via
// contextBridge. Capability modules will extend this surface in M1+.

import { contextBridge, ipcRenderer } from 'electron'
import {
  IpcChannels,
  type BackendProxyRequest,
  type BackendProxyResponse,
} from '../shared/ipc-types'

const api = {
  ping: (): Promise<string> => ipcRenderer.invoke(IpcChannels.AppPing),
  getAppVersion: (): Promise<string> => ipcRenderer.invoke(IpcChannels.AppVersion),
  openExternal: (url: string): Promise<void> => ipcRenderer.invoke(IpcChannels.ShellOpenExternal, url),
  backendProxy: (req: BackendProxyRequest): Promise<BackendProxyResponse> =>
    ipcRenderer.invoke(IpcChannels.BackendProxy, req),
} as const

contextBridge.exposeInMainWorld('agent24', api)

export type Agent24API = typeof api
