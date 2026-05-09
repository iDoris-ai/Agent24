import type { LLMGateway } from './llm-gateway'

export interface ChatMessage {
  role: 'user' | 'assistant' | 'system'
  content: string
}

export interface LLMRequest {
  model?: string
  messages: ChatMessage[]
  stream?: boolean
}

export interface LLMUsage {
  tokens: number
  model: string
  moduleId: string
  timestamp: number
}

export interface CapabilityContext {
  llm: LLMGateway
  moduleId: string
}
