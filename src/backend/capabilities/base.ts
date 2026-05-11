import type { CapabilityContext } from '../types'
import type { ModuleManifest } from '../../shared/ipc-types'

export interface SimpleRouter {
  get(path: string, handler: RouteHandler): void
  post(path: string, handler: RouteHandler): void
}

export interface RouteContext {
  params: Record<string, string>
  query: Record<string, string>
  body: unknown
}

export type RouteHandler = (ctx: RouteContext) => Promise<unknown> | unknown

/**
 * UI module (type='ui'|'hybrid'):
 *   - Declares navItem → shell injects it into sidebar automatically
 *   - Ships a React component loaded lazily by the renderer
 *   - Communicates back to daemon via window.agent24.backendProxy()
 *
 * Headless module (type='headless'):
 *   - Registers daemon-side routes only, no renderer component
 *   - Results surface through chat bubbles or notifications
 */
export interface CapabilityModule {
  manifest: ModuleManifest
  register(router: SimpleRouter, ctx: CapabilityContext): void
}
