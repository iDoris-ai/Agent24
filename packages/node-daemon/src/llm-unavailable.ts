import type { ChatMessage, LLMRequest, LLMUsage } from './types'

export interface ListedModel {
  id: string
  engine?: string
  status?: string
}

/** node-daemon does not call a model server. Desktop and dev chat go through the Rust daemon. */
export class LLMGateway {
  async ensureModels(_models: string[]): Promise<void> {}

  async listModels(): Promise<ListedModel[]> {
    return []
  }

  async chat(_req: LLMRequest, _moduleId: string): Promise<ChatMessage> {
    throw Object.assign(new Error('LLM unavailable in node-daemon: chat goes through the Rust daemon (agent24d)'), { statusCode: 503 })
  }

  getUsage(): LLMUsage[] {
    return []
  }

  clearUsage(): void {}
}
