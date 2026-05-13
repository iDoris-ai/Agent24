import type { CapabilityModule, SimpleRouter } from './capabilities/base'
import type { CapabilityContext } from './types'
import pingModule from './capabilities/example-ping'
import summarizeModule from './capabilities/example-summarize'
import helloUiModule from './capabilities/example-hello-ui'
import codeboxModule from './capabilities/example-codebox'
import { discoverInstalledModules, loadInstalledModule } from './module-installer'

// Built-in bundled modules (always present)
export const MODULES: CapabilityModule[] = [
  pingModule,
  summarizeModule,
  helloUiModule,
  codeboxModule,
]

// Community modules installed at runtime (~/.agent24/modules/)
const _communityModules: CapabilityModule[] = []

// Called once at startup to load previously installed community modules.
export function loadCommunityModules(): void {
  const discovered = discoverInstalledModules()
  for (const { modulePath } of discovered) {
    const mod = loadInstalledModule(modulePath)
    if (mod && !_communityModules.some((m) => m.manifest.id === mod.manifest.id)) {
      _communityModules.push(mod)
      console.log(`[registry] loaded community module: ${mod.manifest.id}`)
    }
  }
}

// Returns all modules (bundled + community) for manifest listing.
export function getAllModules(): CapabilityModule[] {
  return [...MODULES, ..._communityModules]
}

// Dynamically register a single newly installed community module.
// Returns false if the module id is already registered.
// Fires ensureModels() for any declared model requirements (non-blocking).
export function registerCommunityModule(
  mod: CapabilityModule,
  routerFactory: (moduleId: string) => SimpleRouter,
  llmCtx: Omit<CapabilityContext, 'moduleId'>,
): boolean {
  if (getAllModules().some((m) => m.manifest.id === mod.manifest.id)) {
    return false
  }
  _communityModules.push(mod)
  const router = routerFactory(mod.manifest.id)
  mod.register(router, { ...llmCtx, moduleId: mod.manifest.id })
  // M3: ensure declared models are loaded — fire and forget, don't block registration
  if (mod.manifest.models?.length) {
    void llmCtx.llm.ensureModels(mod.manifest.models).catch((err) => {
      console.warn(`[registry] ensureModels failed for ${mod.manifest.id}:`, err)
    })
  }
  return true
}

// Remove a community module from the registry (routes stay until restart).
export function unregisterCommunityModule(moduleId: string): boolean {
  const idx = _communityModules.findIndex((m) => m.manifest.id === moduleId)
  if (idx === -1) return false
  _communityModules.splice(idx, 1)
  return true
}

export function registerAll(
  routerFactory: (moduleId: string) => SimpleRouter,
  llmCtx: Omit<CapabilityContext, 'moduleId'>,
): void {
  for (const mod of getAllModules()) {
    const router = routerFactory(mod.manifest.id)
    const ctx: CapabilityContext = { ...llmCtx, moduleId: mod.manifest.id }
    mod.register(router, ctx)
  }
}
