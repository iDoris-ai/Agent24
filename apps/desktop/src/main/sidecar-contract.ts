import { randomUUID } from 'node:crypto'

/** Dynamic values are kept in main-process memory and never serialized to the renderer. */
export interface SidecarEndpoint {
  readonly origin: string
  readonly secret: string
}

export interface SidecarEndpointHandoff {
  publish(instanceId: string, endpoint: SidecarEndpoint): void
  clear(instanceId: string): void
  current(): { readonly instanceId: string; readonly endpoint: SidecarEndpoint } | null
}

/** Host-only endpoint store; a stale child cannot clear a newer child's endpoint. */
export class MemoryEndpointHandoff implements SidecarEndpointHandoff {
  private value: { instanceId: string; endpoint: SidecarEndpoint } | null = null

  publish(instanceId: string, endpoint: SidecarEndpoint): void {
    this.value = { instanceId, endpoint: { ...endpoint } }
  }

  clear(instanceId: string): void {
    if (this.value?.instanceId === instanceId) this.value = null
  }

  current(): { readonly instanceId: string; readonly endpoint: SidecarEndpoint } | null {
    if (!this.value) return null
    return { instanceId: this.value.instanceId, endpoint: { ...this.value.endpoint } }
  }
}

export interface SidecarOwnership {
  readonly sidecarId: string
  readonly instanceId: string
  readonly pid: number
  readonly processGroupId: number | null
  readonly startedAt: number
}

/** `processGroupId` is evidence from the launcher that a dedicated group exists. */
export function createOwnership(
  sidecarId: string,
  pid: number,
  processGroupId: number | null = null,
): SidecarOwnership {
  return {
    sidecarId,
    instanceId: randomUUID(),
    pid,
    processGroupId,
    startedAt: Date.now(),
  }
}

export type SidecarState = 'stopped' | 'starting' | 'ready' | 'healthy' | 'degraded' | 'stopping' | 'failed'

export interface SidecarStatus {
  readonly state: SidecarState
  readonly sidecarId: string
  readonly instanceId?: string
  readonly message?: string
}
