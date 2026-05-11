import type { LLMGateway } from './llm-gateway'
export type { ModuleManifest, ModuleType, Permission } from '../shared/ipc-types'

// ── LLM ──────────────────────────────────────────────────────────────────────

export interface ChatMessage {
  role: 'user' | 'assistant' | 'system'
  content: string
}

export interface LLMRequest {
  model?: string
  messages: ChatMessage[]
  stream?: boolean
}

export type LLMProvider = 'omlx' | 'ollama' | 'claude' | 'openai'

export interface LLMUsage {
  tokens: number
  model: string
  provider: LLMProvider
  moduleId: string
  timestamp: number
}

// ── Capability context ────────────────────────────────────────────────────────

export interface CapabilityContext {
  llm: LLMGateway
  moduleId: string
}
