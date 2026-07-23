// Module manifest types owned by the daemon.
// MIRROR NOTICE: apps/desktop/src/shared/ipc-types.ts keeps an identical copy
// for IPC typing; the single machine-readable truth is protocol/module.schema.json.
// Unification via the generated api-client lands in task A6 — until then keep
// the two copies in lockstep.

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
