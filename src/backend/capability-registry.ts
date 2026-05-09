import type { CapabilityModule, SimpleRouter } from './capabilities/base'
import type { CapabilityContext } from './types'
import pingModule from './capabilities/example-ping'

const MODULES: CapabilityModule[] = [pingModule]

export function registerAll(router: SimpleRouter, llmCtx: Omit<CapabilityContext, 'moduleId'>): void {
  for (const mod of MODULES) {
    const ctx: CapabilityContext = { ...llmCtx, moduleId: mod.id }
    mod.register(router, ctx)
  }
}
