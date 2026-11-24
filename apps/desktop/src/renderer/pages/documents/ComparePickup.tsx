// L6-S05: the comparison a conversation card asked for, shown on the
// documents page (the page itself only renders this; see doc-focus).

import { useEffect, useState, useSyncExternalStore } from 'react'
import DocumentCompare from './DocumentCompare'
import { clearCompareFocus, pendingCompareFocus, subscribeDocFocus, type CompareFocus } from './doc-focus'

export default function ComparePickup({ api }: { api: Window['agent24']['documents'] }) {
  const pending = useSyncExternalStore(subscribeDocFocus, pendingCompareFocus)
  const [shown, setShown] = useState<CompareFocus | null>(null)
  useEffect(() => {
    if (!pending) return
    clearCompareFocus()
    setShown(pending)
  }, [pending])
  if (!shown) return null
  return <DocumentCompare api={api} left={shown.left} right={shown.right} focusId={shown.focusId} onClose={() => setShown(null)} />
}
