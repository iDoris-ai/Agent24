import type { CapabilityModule, SimpleRouter } from './base'
import type { CapabilityContext } from '../types'

// UI module — has navItem, so shell injects it into sidebar.
// The React component lives at src/renderer/pages/modules/HelloModule.tsx.
// This daemon side registers the API routes the component calls via backendProxy.
const helloUiModule: CapabilityModule = {
  manifest: {
    id: '@auraaihq/example-hello',
    version: '0.1.0',
    name: 'Hello Module',
    description: 'Reference UI module — demonstrates the UI module pattern',
    type: 'ui',
    permissions: ['llm'],
    navItem: {
      icon: '👋',
      label: 'Hello',
      route: '/modules/hello',
    },
  },

  register(router: SimpleRouter, ctx: CapabilityContext): void {
    // Greeting endpoint — the React component calls this via backendProxy
    router.post('/api/modules/hello/greet', async (routeCtx) => {
      const body = routeCtx.body as { name?: string }
      const name = body.name ?? 'stranger'
      const message = await ctx.llm.chat(
        {
          messages: [
            { role: 'system', content: 'You are a friendly assistant. Reply with a short, warm greeting in 1-2 sentences.' },
            { role: 'user', content: `Say hello to ${name}` },
          ],
        },
        ctx.moduleId,
      )
      return { greeting: message.content }
    })

    router.get('/api/modules/hello/info', () => ({
      moduleId: ctx.moduleId,
      version: '0.1.0',
      description: 'Reference UI module showing the CapabilityModule pattern',
    }))
  },
}

export default helloUiModule
