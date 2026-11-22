// F3: a source a card in the conversation points at (「第 2 页」): the
// documents page opens that document on its page, the block highlighted.

export interface DocFocus { documentId: string; page?: number; blockId: string }

let pending: DocFocus | null = null
const listeners = new Set<() => void>()

export function subscribeDocFocus(listener: () => void): () => void {
  listeners.add(listener)
  return () => { listeners.delete(listener) }
}

export function focusDocument(focus: DocFocus): void {
  pending = focus
  for (const l of listeners) l()
}

export function pendingDocFocus(): DocFocus | null {
  return pending
}

/** The documents page has taken it. */
export function clearDocFocus(): void {
  pending = null
}
