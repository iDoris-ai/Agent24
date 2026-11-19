import type { CommHistoryItem } from './api'

/** Excludes protocol traffic and messages the local identity sent to itself. */
export function isHumanCommMessage(item: CommHistoryItem): boolean {
  const body = item.content.trim()
  if (body.startsWith('a24-liveness-canary') || body.startsWith('agent24.friend/v1')) return false
  if (item.from && item.from === item.to) return false
  try {
    const value: unknown = JSON.parse(body)
    if (typeof value === 'object' && value !== null && !Array.isArray(value)) {
      const record = value as Record<string, unknown>
      if (record.version === 'f4/1' || record.protocol === 'hyphae-behavior/1') return false
    }
  } catch { /* ordinary text */ }
  return true
}

export function filterHumanCommMessages(items: CommHistoryItem[]): CommHistoryItem[] {
  return items.filter(isHumanCommMessage)
}
