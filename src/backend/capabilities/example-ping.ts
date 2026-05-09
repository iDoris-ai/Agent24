import type { CapabilityModule, SimpleRouter } from './base'
import type { CapabilityContext } from '../types'

const pingModule: CapabilityModule = {
  id: 'ping',

  register(router: SimpleRouter, _ctx: CapabilityContext): void {
    router.get('/api/capabilities/ping', () => ({
      status: 'ok',
      moduleId: 'ping',
      ts: Date.now(),
    }))
  },
}

export default pingModule
