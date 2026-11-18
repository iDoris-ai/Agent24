import type { BackendProxyResponse } from '../../shared/ipc-types'

export type KernelConsentClass = 'exempt' | 'missing' | 'granted'

export interface KernelConsentView {
  module: string
  class: KernelConsentClass
  fingerprint: string
  version: string
  team: string | null
  origin: 'bundled' | 'local'
  isolated: boolean
  capabilities: string[]
  consented: string[] | null
}

const CLASSES = new Set<KernelConsentClass>(['exempt', 'missing', 'granted'])

function object(value: unknown): Record<string, unknown> | null {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return null
  return value as Record<string, unknown>
}

function strings(value: unknown): string[] | null {
  if (!Array.isArray(value) || value.some((item) => typeof item !== 'string')) return null
  return value as string[]
}

export function parseKernelConsent(value: unknown): KernelConsentView | null {
  const row = object(value)
  if (!row) return null
  if (typeof row.module !== 'string' || row.module.length === 0) return null
  if (typeof row.class !== 'string' || !CLASSES.has(row.class as KernelConsentClass)) return null
  if (typeof row.fingerprint !== 'string' || typeof row.version !== 'string') return null
  if (row.team !== null && typeof row.team !== 'string') return null
  if (row.origin !== 'bundled' && row.origin !== 'local') return null
  if (typeof row.isolated !== 'boolean') return null
  const capabilities = strings(row.capabilities)
  if (!capabilities) return null
  const consented = row.consented === null ? null : strings(row.consented)
  if (row.consented !== null && consented === null) return null
  return {
    module: row.module,
    class: row.class as KernelConsentClass,
    fingerprint: row.fingerprint,
    version: row.version,
    team: row.team,
    origin: row.origin,
    isolated: row.isolated,
    capabilities,
    consented,
  }
}

export function errorCode(res: BackendProxyResponse): string | undefined {
  const data = object(res.data)
  const error = data ? object(data.error) : null
  return typeof error?.code === 'string' ? error.code : undefined
}

export async function loadKernelConsent(name: string): Promise<KernelConsentView | null> {
  const res = await window.agent24.backendProxy({
    method: 'GET',
    path: `/api/v1/os/${encodeURIComponent(name)}/kernel-consent`,
  })
  if (!res.ok) return null
  return parseKernelConsent(res.data)
}

/** Propose a subset. The daemon writes the row only after the native
 * presence dialog lists these capabilities and the user agrees there. */
export async function proposeKernelConsent(
  name: string,
  fingerprint: string,
  capabilities: readonly string[],
): Promise<BackendProxyResponse> {
  return window.agent24.backendProxy({
    method: 'POST',
    path: `/api/v1/os/${encodeURIComponent(name)}/kernel-consent`,
    body: { fingerprint, capabilities },
  })
}

export async function revokeKernelConsent(name: string): Promise<BackendProxyResponse> {
  return window.agent24.backendProxy({
    method: 'DELETE',
    path: `/api/v1/os/${encodeURIComponent(name)}/kernel-consent`,
  })
}
