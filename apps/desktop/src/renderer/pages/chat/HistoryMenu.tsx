import { useEffect, useLayoutEffect, useRef } from 'react'

// UI-06 G2: the history list's small menus (a conversation's ⋯, a group's
// ⋯), opened by their button or a right click. A menu of buttons: arrows
// move between them, Enter or a click picks one, Escape, Tab or a click
// outside closes it.

export type MenuEntry =
  | { kind: 'item'; label: string; onSelect: () => void; disabled?: boolean }
  | { kind: 'heading'; label: string }

export function HistoryMenu({ label, entries, at, onClose }: {
  label: string
  entries: MenuEntry[]
  /** Where it opens, in viewport pixels. */
  at: { x: number; y: number }
  /** `refocus`: give focus back to what opened it (closed from the keyboard). */
  onClose: (refocus: boolean) => void
}) {
  const ref = useRef<HTMLDivElement>(null)
  const items = () => Array.from(ref.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)') ?? [])
  useLayoutEffect(() => { items()[0]?.focus() }, [])
  useEffect(() => {
    const outside = (e: MouseEvent) => { if (!ref.current?.contains(e.target as Node)) onClose(false) }
    document.addEventListener('mousedown', outside)
    return () => document.removeEventListener('mousedown', outside)
  }, [onClose])
  const move = (step: number | 'first' | 'last') => {
    const list = items()
    if (list.length === 0) return
    const at = list.indexOf(document.activeElement as HTMLButtonElement)
    const next = step === 'first' ? 0 : step === 'last' ? list.length - 1 : (at + step + list.length) % list.length
    list[next].focus()
  }
  return (
    <div ref={ref} role="menu" aria-label={label} className="chat-history-menu"
      style={{ left: at.x, top: at.y }}
      onKeyDown={e => {
        if (e.key === 'ArrowDown') { e.preventDefault(); move(1) }
        else if (e.key === 'ArrowUp') { e.preventDefault(); move(-1) }
        else if (e.key === 'Home') { e.preventDefault(); move('first') }
        else if (e.key === 'End') { e.preventDefault(); move('last') }
        else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); onClose(true) }
        else if (e.key === 'Tab') onClose(false)
      }}>
      {entries.map((entry, i) => entry.kind === 'heading'
        ? <div key={i} className="chat-history-menu-heading" role="presentation">{entry.label}</div>
        : <button key={i} type="button" role="menuitem" className="chat-history-menu-item" disabled={entry.disabled}
          onClick={() => { onClose(false); entry.onSelect() }}>{entry.label}</button>)}
    </div>
  )
}
