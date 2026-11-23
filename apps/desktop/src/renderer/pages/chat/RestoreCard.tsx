// L6-F5: 「回到上一个版本」: the version the App found, restored only when the
// user clicks; the model is not asked.

import type { RestoreCardData } from './doc-version'

export function RestoreCard({ card, onRestore }: { card: RestoreCardData; onRestore?: () => void }) {
  return (
    <div role="group" aria-label="恢复版本" className="extract-card">
      {onRestore && <button type="button" className="btn btn-primary extract-card-source" onClick={onRestore}>恢复第 {card.from} 版</button>}
    </div>
  )
}
