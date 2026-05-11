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
  LlmStatus: 'llm:status',
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
}

export interface LlmStatusResult {
  provider: 'omlx' | 'ollama' | 'none'
  url: string
  model: string
}
