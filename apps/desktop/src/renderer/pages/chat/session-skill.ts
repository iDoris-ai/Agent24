// The skill a desktop conversation will name on every run. The daemon does
// not store this; each RunCreate carries it again. It lives in renderer
// memory only (v1): there is no per-conversation settings store and history
// records are versioned, so a reload starts with no skill chosen.

const selected = new Map<string, string>()
const listeners = new Set<() => void>()

function emit(): void {
  for (const listener of listeners) listener()
}

export function skillForConversation(conversationId: string): string | null {
  return selected.get(conversationId) ?? null
}

export function setConversationSkill(conversationId: string, skillId: string | null): void {
  if (skillId) selected.set(conversationId, skillId)
  else selected.delete(conversationId)
  emit()
}

export function subscribeConversationSkill(listener: () => void): () => void {
  listeners.add(listener)
  return () => { listeners.delete(listener) }
}

export function resetConversationSkillsForTests(): void {
  selected.clear()
  emit()
}

/** The skills page tells the chat picker the active set may have moved. */
export const SKILLS_CHANGED_EVENT = 'agent24:skills-changed'

export function notifySkillsChanged(): void {
  window.dispatchEvent(new Event(SKILLS_CHANGED_EVENT))
}
