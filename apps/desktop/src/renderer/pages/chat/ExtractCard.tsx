// F3「抽取结果」卡: each field, its value as the document writes it, and the
// pages it was read from; a click opens the page with the block highlighted.
// A field the extractor did not find says so; nothing is filled in for it.

import { useContext } from 'react'
import { CapabilitiesContext } from '../../capabilities/types'
import type { ExtractCardData } from '../documents/doc-extract'
import { focusDocument } from '../documents/doc-focus'

/** `onAnswer`: not an extraction after all; the model answers the same words. */
export function ExtractCard({ card, onAnswer }: { card: ExtractCardData; onAnswer?: () => void }) {
  const { open } = useContext(CapabilitiesContext)
  return (
    <div role="group" aria-label="抽取结果" className="extract-card">
      <div className="extract-card-title">抽取结果 · 《{card.title}》{card.converted ? '（按转换出的 PDF）' : ''}</div>
      <table className="extract-card-table">
        <thead>
          <tr><th>字段</th><th>值 · 出处</th></tr>
        </thead>
        <tbody>
          {card.rows.map((r) => (
            <tr key={r.name}>
              <td className="extract-card-name">{r.name}</td>
              <td>
                {r.found.length === 0 && <span className="extract-card-muted">文档里没找到</span>}
                {r.found.length > 1 && <div className="extract-card-muted">文档里有 {r.found.length} 种写法，未替你选：</div>}
                {r.found.map((f, i) => (
                  <div key={i} className="extract-card-value">
                    {f.value}
                    {f.sources.map((s, j) => (
                      <button key={`${s.block_id}-${j}`} type="button" className="btn btn-ghost extract-card-source" title={s.quote}
                        onClick={() => { focusDocument({ documentId: card.documentId, page: s.page, blockId: s.block_id }); open('documents', 'other') }}>
                        {s.page !== undefined ? `${card.converted ? '转换后' : ''}第 ${s.page} 页` : '原文'}
                      </button>
                    ))}
                  </div>
                ))}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {onAnswer && (
        <button type="button" className="btn btn-ghost extract-card-answer" onClick={onAnswer}>不是要抽取？让助手回答</button>
      )}
    </div>
  )
}
