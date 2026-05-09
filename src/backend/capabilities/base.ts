// CapabilityModule interface — every capability registers its routes via this contract.
// M3: replace SimpleRouter with FastifyInstance once Fastify is introduced.

import type { CapabilityContext } from '../types'

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

export interface CapabilityModule {
  id: string
  register(router: SimpleRouter, ctx: CapabilityContext): void
}
