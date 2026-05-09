// Shared IPC channel names and payload types between main and renderer.
// Capability modules will extend this in M1+.

export const IpcChannels = {
  AppPing: 'app:ping',
  AppVersion: 'app:version',
  // Opens a URL in the system browser via shell.openExternal (main process).
  // Renderer must never open external URLs directly — always route via this.
  ShellOpenExternal: 'shell:open-external',
  // Proxies an HTTP request to the backend daemon (localhost:8765).
  // Renderer never calls the daemon directly — always goes through this channel.
  BackendProxy: 'backend:proxy',
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
