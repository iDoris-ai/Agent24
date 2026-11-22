// F3「抽取结果」卡: each field, its value as the document writes it, and the
// pages it was read from; a click opens the page with the block highlighted.
// A field the extractor did not find says so; nothing is filled in for it.

import { useContext } from 'react'
import { CapabilitiesContext } from '../../capabilities/types'
import type { ExtractCardData } from '../documents/doc-extract'
import { focusDocument } from '../documents/doc-focus'

export function ExtractCard({ card }: { card: ExtractCardData }) {
  const { open } = useContext(CapabilitiesContext)
  return (
    <div role="group" aria-label="抽取结果" className="extract-card" style={{ border: '1px solid var(--border)', borderRadius: 8, padding: '8px 12px', marginTop: 8 }}>
      <div style={{ fontWeight: 600, fontSize: 13, marginBottom: 4 }}>抽取结果 · 《{card.title}》</div>
      <table style={{ borderCollapse: 'collapse', fontSize: 13, width: '100%' }}>
        <thead>
          <tr style={{ color: 'var(--muted)', textAlign: 'left' }}><th>字段</th><th>值</th><th>出处</th></tr>
        </thead>
        <tbody>
          {card.rows.map((r) => (
            <tr key={r.name} style={{ borderTop: '1px solid var(--border)', verticalAlign: 'top' }}>
              <td style={{ padding: '4px 8px 4px 0', whiteSpace: 'nowrap' }}>{r.name}</td>
              <td style={{ padding: '4px 8px 4px 0' }}>
                {r.values.length === 0
                  ? <span style={{ color: 'var(--muted)' }}>文档里没找到</span>
                  : r.values.map((v, i) => <div key={i}>{v}</div>)}
                {r.values.length > 1 && <div style={{ color: 'var(--muted)', fontSize: 12 }}>文档里写法不一致，未替你选</div>}
              </td>
              <td style={{ padding: '4px 0' }}>
                {r.sources.map((s, i) => (
                  <button key={`${s.block_id}-${i}`} type="button" className="btn btn-ghost" title={s.quote} style={{ fontSize: 12, padding: '0 4px' }}
                    onClick={() => { focusDocument({ documentId: card.documentId, page: s.page, blockId: s.block_id }); open('documents', 'other') }}>
                    {s.page !== undefined ? `第 ${s.page} 页` : '原文'}
                  </button>
                ))}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}
