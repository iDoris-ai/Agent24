// CodeBox capability module — Python sandbox powered by BoxLite.
// Exposes POST /api/codebox/run  { code: string } → { ok, output?, error? }
// and     GET  /api/codebox/status → { available, error? }

import type { CapabilityModule, SimpleRouter } from './base'
import type { CapabilityContext } from '../types'
import { runPython, isBoxliteAvailable, getBoxliteError } from '../boxlite-host'

const codeboxModule: CapabilityModule = {
  manifest: {
    id: 'codebox',
    version: '0.1.0',
    name: 'Python 沙箱',
    description: 'BoxLite 驱动的隔离 Python 执行环境',
    type: 'hybrid',
    permissions: [],
    navItem: {
      icon: '🐍',
      label: 'Python 沙箱',
      route: 'codebox',
    },
  },

  register(router: SimpleRouter, _ctx: CapabilityContext): void {
    router.get('/api/codebox/status', () => ({
      available: isBoxliteAvailable(),
      error: getBoxliteError(),
    }))

    router.post('/api/codebox/run', async (ctx) => {
      const body = ctx.body as { code?: string }
      const code = body?.code
      if (typeof code !== 'string' || !code.trim()) {
        throw Object.assign(new Error('code (string) required'), { statusCode: 400 })
      }
      return runPython(code)
    })
  },
}

export default codeboxModule
