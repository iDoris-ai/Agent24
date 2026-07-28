import { describe, it, expect, vi, beforeEach } from 'vitest'
import type { CapabilityModule } from './capabilities/base'

// Capture the container start (whose startCmd is arbitrary code) — the side
// effect H10 must NOT fire for a not-yet-consented module.
const { startService, stopService } = vi.hoisted(() => ({
  startService: vi.fn(async () => ({ ok: true, hostPort: 9000 })),
  stopService: vi.fn(async () => ({ ok: true })),
}))
vi.mock('./boxlite-service', () => ({
  startService,
  stopService,
  proxyToService: vi.fn(),
  getHostPort: vi.fn(),
  stopAll: vi.fn(),
}))

// Control the consent gate directly, without touching the on-disk state file.
const consent = vi.hoisted(() => ({ enabled: true }))
vi.mock('./module-state', () => ({ isEnabled: () => consent.enabled }))

import {
  registerCommunityModule,
  startModuleServices,
  unregisterCommunityModule,
} from './capability-registry'

function containerModule(id: string): CapabilityModule {
  return {
    manifest: {
      id,
      version: '0.1.0',
      name: id,
      description: 'declares a container',
      type: 'headless',
      permissions: [],
      container: { image: 'busybox', startCmd: 'echo started' },
    },
    register: () => {},
  } as unknown as CapabilityModule
}

const noopRouter = () => ({}) as never
const llmCtx = { llm: {} as never }

describe('H10: a module container starts only after consent', () => {
  beforeEach(() => {
    startService.mockClear()
    stopService.mockClear()
  })

  it('does NOT run the container startCmd when the module is disabled (pending consent)', () => {
    consent.enabled = false
    registerCommunityModule(containerModule('mod-pending'), noopRouter, llmCtx)
    // This is the exact regression: registration must be inert for a disabled
    // module — no startService, so no arbitrary startCmd executed on install.
    expect(startService).not.toHaveBeenCalled()
    unregisterCommunityModule('mod-pending')
  })

  it('runs the container startCmd only once the user enables (consents)', () => {
    consent.enabled = true
    const mod = containerModule('mod-consented')
    // The enable-toggle path starts the services registration deliberately skipped.
    startModuleServices(mod, llmCtx)
    expect(startService).toHaveBeenCalledOnce()
    expect(startService).toHaveBeenCalledWith('mod-consented', mod.manifest.container)
  })

  it('an enabled module starts its container at registration, as before', () => {
    consent.enabled = true
    registerCommunityModule(containerModule('mod-enabled'), noopRouter, llmCtx)
    expect(startService).toHaveBeenCalledOnce()
    unregisterCommunityModule('mod-enabled')
  })
})
