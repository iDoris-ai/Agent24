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

// L6-S05: a line of a comparison summary in the conversation opens the
// comparison view on the documents page, at that change.
export interface CompareFocus {
  left: { documentId: string; revision?: number; label: string }
  right: { documentId: string; revision?: number; label: string }
  focusId?: string
}

let pendingCompare: CompareFocus | null = null

export function openCompare(focus: CompareFocus): void {
  pendingCompare = focus
  for (const l of listeners) l()
}

export function pendingCompareFocus(): CompareFocus | null {
  return pendingCompare
}

/** The documents page has taken it. */
export function clearCompareFocus(): void {
  pendingCompare = null
}
