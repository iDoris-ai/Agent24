// L7-K1: the citation card under a Knowledge answer: where the search found
// passages, library by library, 「库名 · 文档名 · 位置」, from the run's record
// (`knowledge.found`). It says what was found, not which passage the answer
// used. It carries no passage text: the original is read on the knowledge
// page, which the card opens; 「复制出处」 copies the lines above, never a
// quotation (a search snippet is a clue, not a quotation: READONLY R3.8).
// Only libraries the settings bind are named; nothing here reaches another.

import { useContext, useEffect, useState } from 'react'
import { CapabilitiesContext } from '../../capabilities/types'
import { useT } from '../../i18n'
import type { RunKnowledge } from '../knowledge-grounding'
import { citationMessages } from './KnowledgeCitationCard.i18n'
import { libraryLabel, loadKbNames, type KbNames } from './kb-names'

type Found = NonNullable<RunKnowledge['found']>

export interface CitationLine { library: string; title: string; place: number; libraryId: string }

/** Lines grouped by library in the order the search ranked them; a document's
 * n-th hit is its 「第 n 段命中」. */
export function citationLines(found: Found, bound: readonly string[], names: KbNames, numbered: string, plain: string): CitationLine[] {
  const perDoc = new Map<string, number>()
  const lines = found.map((h) => {
    const key = `${h.knowledge_base_id}\u0000${h.knowledge_id}`
    const place = (perDoc.get(key) ?? 0) + 1
    perDoc.set(key, place)
    return { library: libraryLabel(h.knowledge_base_id, bound, names, numbered, plain), title: h.title, place, libraryId: h.knowledge_base_id }
  })
  const order = [...new Set(lines.map((l) => l.libraryId))]
  return order.flatMap((id) => lines.filter((l) => l.libraryId === id))
}

export default function KnowledgeCitationCard({ found, bound: given, names: givenNames }: { found: RunKnowledge['found']; bound?: readonly string[]; names?: KbNames }) {
  const t = useT(citationMessages)
  const { open } = useContext(CapabilitiesContext)
  const [bound, setBound] = useState<readonly string[]>(given ?? [])
  const [copied, setCopied] = useState(false)
  // Loop 9 3c: the local Lite has no knowledge page (a headless build), so the card does not offer to open one.
  const [local, setLocal] = useState(false)
  useEffect(() => {
    let live = true
    void (async () => {
      try {
        const s = await window.agent24.knowledge?.status()
        if (!live || !s) return
        if (!given) setBound(s.source === 'local' ? s.activeKnowledgeBaseIds : s.knowledgeBaseIds)
        setLocal(s.source === 'local')
      } catch { /* no ids: the libraries read 「知识库」 */ }
    })()
    return () => { live = false }
  }, [given])
  if (!found || found.length === 0) return null
  const lines = citationLines(found, bound, givenNames ?? loadKbNames(), t('library'), t('plain'))
  const text = (l: CitationLine) => `${l.library} · ${l.title === '' ? t('untitled') : `《${l.title}》`} · ${t('place', { n: String(l.place) })}`
  const copy = async () => {
    try {
      await navigator.clipboard.writeText([t('copyHeader'), ...lines.map(text)].join('\n'))
      setCopied(true)
    } catch { /* the clipboard is not available: nothing to say it copied */ }
  }
  return (
    <div role="group" aria-label={t('title')} className="task-card-knowledge">
      <div>{t('title')}</div>
      <ul>{lines.map((l, i) => <li key={i}>{text(l)}</li>)}</ul>
      <div>{t('note')}</div>
      {!local && <button type="button" className="btn btn-ghost" onClick={() => open('knowledgeBase', 'other')}>{t('open')}</button>}
      <button type="button" className="btn btn-ghost" onClick={() => void copy()}>{copied ? t('copied') : t('copy')}</button>
    </div>
  )
}
