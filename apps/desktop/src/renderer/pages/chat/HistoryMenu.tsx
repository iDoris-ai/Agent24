import { useEffect, useLayoutEffect, useRef, useState } from 'react'

// UI-06 G2: the history list's small menus (a conversation's ⋯, a group's
// ⋯), opened by their button or a right click. A menu of buttons: arrows
// move between them, Enter or a click picks one, Escape, Tab or a click
// outside closes it.

export type MenuEntry =
  | {
    kind: 'item'
    /** Stable across renders, so the entry under focus stays the same action
     *  when the menu's entries change while it is open. */
    key: string
    label: string
    /** What it does; a promise when the list changes once main answers. */
    onSelect: () => void | Promise<unknown>
    disabled?: boolean
    /** It opens a box that takes the focus itself. */
    focuses?: boolean
  }
  | { kind: 'heading'; key: string; label: string }

const EDGE = 4

export function HistoryMenu({ label, entries, at, onClose }: {
  label: string
  entries: MenuEntry[]
  /** Where it opens, in viewport pixels. */
  at: { x: number; y: number }
  /** `refocus`: give focus back to the list (closed from the keyboard),
   *  once `after` (the action picked) has finished. */
  onClose: (refocus: boolean, after?: Promise<unknown>) => void
}) {
  const ref = useRef<HTMLDivElement>(null)
  const items = () => Array.from(ref.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)') ?? [])
  // Kept inside the window: moved up when it would run past the bottom,
  // left when past the right edge, and never taller than the window.
  const [pos, setPos] = useState(at)
  useLayoutEffect(() => {
    const box = ref.current?.getBoundingClientRect()
    if (!box) return
    const x = Math.max(EDGE, Math.min(at.x, window.innerWidth - box.width - EDGE))
    const y = at.y + box.height > window.innerHeight - EDGE ? Math.max(EDGE, window.innerHeight - box.height - EDGE) : at.y
    setPos(p => (p.x === x && p.y === y ? p : { x, y }))
  }, [at.x, at.y, entries.length])
  useLayoutEffect(() => { items()[0]?.focus() }, [])
  useEffect(() => {
    const outside = (e: MouseEvent) => { if (!ref.current?.contains(e.target as Node)) onClose(false) }
    // A resized window would leave it where it no longer fits: it closes.
    const resized = () => onClose(false)
    document.addEventListener('mousedown', outside)
    window.addEventListener('resize', resized)
    return () => {
      document.removeEventListener('mousedown', outside)
      window.removeEventListener('resize', resized)
    }
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
      style={{ left: pos.x, top: pos.y }}
      onKeyDown={e => {
        if (e.key === 'ArrowDown') { e.preventDefault(); move(1) }
        else if (e.key === 'ArrowUp') { e.preventDefault(); move(-1) }
        else if (e.key === 'Home') { e.preventDefault(); move('first') }
        else if (e.key === 'End') { e.preventDefault(); move('last') }
        else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); onClose(true) }
        else if (e.key === 'Tab') onClose(false)
      }}>
      {entries.map(entry => entry.kind === 'heading'
        ? <div key={entry.key} className="chat-history-menu-heading" role="presentation">{entry.label}</div>
        : <button key={entry.key} type="button" role="menuitem" className="chat-history-menu-item" disabled={entry.disabled}
          onClick={e => {
            // Picked from the keyboard (a click with no pointer behind it):
            // focus goes back to the list once the action is done, unless
            // the action opens a box of its own.
            const after = entry.onSelect()
            onClose(e.detail === 0 && !entry.focuses, after ? Promise.resolve(after) : undefined)
          }}>{entry.label}</button>)}
    </div>
  )
}
