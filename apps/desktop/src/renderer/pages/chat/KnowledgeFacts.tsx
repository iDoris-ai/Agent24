// L7-K0: the fact line under a Knowledge answer, from the run's record:
// 「已检索知识库：找到 N 段（《…》），可展开」, 「没找到」, or 「检索没能进行」.
// It says what the search found, never which passage the answer used.

import { useT } from '../../i18n'
import { chatMessages } from '../Chat.i18n'
import type { KnowledgeFacts as Facts } from '../knowledge-grounding'

export default function KnowledgeFacts({ facts }: { facts: Facts | null | undefined }) {
  const t = useT(chatMessages)
  if (!facts) return null
  if (facts.kind === 'refused') return <div role="note" className="task-card-knowledge">{t(facts.reason === 'policy' ? 'knowledge.facts.refused' : 'knowledge.facts.refusedFailed')}</div>
  if (facts.kind === 'none') return <div role="note" className="task-card-knowledge">{t('knowledge.facts.none')}</div>
  const names = facts.titles.map((x) => `《${x}》`).join('')
  return (
    <details className="message-detail task-card-knowledge">
      <summary>{names ? t('knowledge.facts.found', { n: String(facts.passages), titles: names }) : t('knowledge.facts.foundNoTitle', { n: String(facts.passages) })}</summary>
      <div className="message-detail-list">
        {facts.titles.map((x, i) => <div key={i}>{`《${x}》`}</div>)}
        <div>{t('knowledge.facts.note')}</div>
      </div>
    </details>
  )
}
