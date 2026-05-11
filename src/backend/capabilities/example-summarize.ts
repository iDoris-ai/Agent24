import type { CapabilityModule, SimpleRouter } from './base'
import type { CapabilityContext } from '../types'

// Headless module — no UI, no navItem.
// Registers POST /api/capabilities/summarize.
// Daemon calls LLM gateway; results surface via chat or any caller using backendProxy.
const summarizeModule: CapabilityModule = {
  manifest: {
    id: '@auraaihq/example-summarize',
    version: '0.1.0',
    name: 'Summarize',
    description: 'Summarize any text via the active LLM provider',
    type: 'headless',
    permissions: ['llm'],
  },

  register(router: SimpleRouter, ctx: CapabilityContext): void {
    router.post('/api/capabilities/summarize', async (routeCtx) => {
      const body = routeCtx.body as { text?: string; language?: string }
      if (!body.text || typeof body.text !== 'string') {
        throw Object.assign(new Error('text field required'), { statusCode: 400 })
      }
      const lang = body.language ?? 'same language as input'
      const message = await ctx.llm.chat(
        {
          messages: [
            {
              role: 'system',
              content: `You are a concise summarizer. Summarize in ${lang}. Return only the summary, no preamble.`,
            },
            { role: 'user', content: body.text },
          ],
        },
        ctx.moduleId,
      )
      return { summary: message.content }
    })
  },
}

export default summarizeModule
