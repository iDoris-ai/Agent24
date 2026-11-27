// L6-F2: several documents answer to the name a delete turn gave. Each is
// listed as the library has it (title, import time, pages), never by id; the
// one picked is deleted through the usual approval card.

import type { DeleteCandidate } from './delete-target'

export function DeleteChoice({ candidates, onPick }: { candidates: DeleteCandidate[]; onPick?: (c: DeleteCandidate, n: number) => void }) {
  return (
    <div role="group" aria-label="选择要删除的文档" className="extract-card">
      {candidates.map((c, i) => (
        <div key={c.id} className="extract-card-value" style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
          <span>{i + 1}. 《{c.title}》</span>
          <span className="extract-card-muted">导入于 {c.importedAt}{c.pages !== null ? ` · 共 ${c.pages} 页` : ''}</span>
          {onPick && <button type="button" className="btn btn-ghost extract-card-source" onClick={() => onPick(c, i + 1)}>删除这一份</button>}
        </div>
      ))}
    </div>
  )
}
