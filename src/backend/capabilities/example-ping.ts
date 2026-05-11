import type { CapabilityModule, SimpleRouter } from './base'
import type { CapabilityContext } from '../types'

const pingModule: CapabilityModule = {
  manifest: {
    id: 'ping',
    version: '0.1.0',
    name: 'Ping',
    description: 'Health check example module',
    type: 'headless',
    permissions: [],
  },

  register(router: SimpleRouter, _ctx: CapabilityContext): void {
    router.get('/api/capabilities/ping', () => ({
      status: 'ok',
      moduleId: 'ping',
      ts: Date.now(),
    }))
  },
}

export default pingModule
