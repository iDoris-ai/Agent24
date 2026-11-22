import { describe, expect, it } from 'vitest'
import { chatMessages } from './Chat.i18n'

// UI polish 04 (David): what the assistant page tells the user carries no
// program names, commands or ids: 「agent24 tui」「agent24d」「schema」「agent 模式」.
const KEYS = ['task.decideInTui', 'knowledge.via', 'knowledge.inactive', 'reply.backendNotReady'] as const
const INTERNAL = /agent24d|daemon|schema|agent24 (tui|service)|agent 模式|agent mode|โหมด agent/i

describe('assistant page copy', () => {
  for (const locale of ['en', 'zh', 'th'] as const) {
    it.each(KEYS)(`${locale} %s says nothing internal`, (key) => {
      expect(chatMessages[locale][key]).not.toMatch(INTERNAL)
    })
  }
})
