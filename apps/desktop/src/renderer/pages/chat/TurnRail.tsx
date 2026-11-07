import { useEffect, useId, useRef, useState } from 'react'
import { useT } from '../../i18n'
import { chatMessages } from '../Chat.i18n'
import type { Turn } from './turns'

/** Characters of the reply shown on a turn's card. */
const REPLY_PREVIEW_CHARS = 80

function preview(text: string): string {
  const flat = text.replace(/\s+/g, ' ').trim()
  return flat.length > REPLY_PREVIEW_CHARS ? `${flat.slice(0, REPLY_PREVIEW_CHARS)}…` : flat
}

/** A column of marks at the left edge of the chat, one per turn. Hovering
 *  or focusing a mark shows that turn's prompt and the start of its reply;
 *  clicking it jumps there. The mark of the turn being read is highlighted
 *  and kept in view when the rail scrolls. The card sits outside the marks'
 *  scroll box so a long rail never clips it; the pointer may move onto it,
 *  and Escape closes it. */
export function TurnRail({ turns, active, onSelect }: {
  turns: readonly Turn[]
  active: number
  onSelect: (turn: number) => void
}) {
  const t = useT(chatMessages)
  const cardId = useId()
  const railRef = useRef<HTMLDivElement>(null)
  const activeRef = useRef<HTMLButtonElement | null>(null)
  const [shown, setShown] = useState<{ turn: number; top: number } | null>(null)
  const show = (turn: number, mark: HTMLElement) => {
    const rail = railRef.current?.getBoundingClientRect()
    setShown({ turn, top: rail ? mark.getBoundingClientRect().top - rail.top : 0 })
  }
  useEffect(() => {
    activeRef.current?.scrollIntoView?.({ block: 'nearest' })
  }, [active, turns.length])
  const card = shown === null ? undefined : turns[shown.turn]
  return (
    <div className="turn-rail" ref={railRef} onMouseLeave={() => setShown(null)}
      onKeyDown={e => { if (e.key === 'Escape') setShown(null) }}>
      <nav className="turn-rail-marks" aria-label={t('turns.label')}>
        {turns.map((turn, i) => (
          <button
            key={turn.messageIndex}
            ref={i === active ? activeRef : undefined}
            type="button"
            className={`turn-rail-mark${i === active ? ' active' : ''}`}
            aria-label={t('turns.mark', { n: i + 1, prompt: turn.prompt })}
            aria-current={i === active ? 'step' : undefined}
            aria-describedby={shown?.turn === i ? cardId : undefined}
            onMouseEnter={e => show(i, e.currentTarget)}
            onFocus={e => show(i, e.currentTarget)}
            onBlur={() => setShown(null)}
            onClick={() => onSelect(i)}
          />
        ))}
      </nav>
      {shown !== null && card && (
        <div id={cardId} className="turn-card" role="tooltip" style={{ top: shown.top }}>
          <div className="turn-card-prompt">{card.prompt}</div>
          <div className="turn-card-reply">{card.reply === null ? t('turns.noReply') : preview(card.reply)}</div>
        </div>
      )}
    </div>
  )
}
