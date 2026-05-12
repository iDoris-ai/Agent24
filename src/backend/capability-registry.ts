import type { CapabilityModule, SimpleRouter } from './capabilities/base'
import type { CapabilityContext } from './types'
import pingModule from './capabilities/example-ping'
import summarizeModule from './capabilities/example-summarize'
import helloUiModule from './capabilities/example-hello-ui'

export const MODULES: CapabilityModule[] = [
  pingModule,
  summarizeModule,
  helloUiModule,
]

export function registerAll(
  routerFactory: (moduleId: string) => SimpleRouter,
  llmCtx: Omit<CapabilityContext, 'moduleId'>,
): void {
  for (const mod of MODULES) {
    const router = routerFactory(mod.manifest.id)
    const ctx: CapabilityContext = { ...llmCtx, moduleId: mod.manifest.id }
    mod.register(router, ctx)
  }
}
