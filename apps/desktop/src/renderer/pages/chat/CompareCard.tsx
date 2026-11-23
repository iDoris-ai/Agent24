// L6-S05: the summary of a comparison, a line per change; a click opens the
// comparison view on the documents page at that change. The lines are the
// module's facts written by the App (doc-compare.ts), not the model's.

import { useContext } from 'react'
import { CapabilitiesContext } from '../../capabilities/types'
import { openCompare } from '../documents/doc-focus'
import type { CompareCardData } from './doc-compare'

export function CompareCard({ card }: { card: CompareCardData }) {
  const { open } = useContext(CapabilitiesContext)
  const go = (focusId?: string) => { openCompare({ left: card.left, right: card.right, ...(focusId ? { focusId } : {}) }); open('documents', 'other') }
  return (
    <div role="group" aria-label="文档对比" className="extract-card">
      {card.lines.map((l) => (
        <button key={l.id} type="button" className="btn btn-ghost extract-card-source" onClick={() => go(l.id)}>{l.text}</button>
      ))}
      <button type="button" className="btn btn-primary extract-card-source" onClick={() => go()}>打开对比视图</button>
    </div>
  )
}
