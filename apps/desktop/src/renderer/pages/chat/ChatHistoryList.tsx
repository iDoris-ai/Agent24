import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react'
import { useT } from '../../i18n'
import { chatMessages } from '../Chat.i18n'
import {
  clearConversations,
  deleteConversation,
  getHistoryState,
  openConversation,
  refreshHistory,
  renameConversation,
  subscribeHistory,
} from './history'
import { graphemes, oneLine, renameTitle } from './title'
import type { ChatHistorySummary } from '../../../shared/chat-history'

/** UI-04 T3: with more conversations than this, the list offers a search. */
export const SEARCH_FROM = 8

/** Local time as `MM-DD HH:mm`, short enough for the sidebar. */
export function shortTime(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  const two = (n: number) => String(n).padStart(2, '0')
  return `${two(d.getMonth() + 1)}-${two(d.getDate())} ${two(d.getHours())}:${two(d.getMinutes())}`
}

/** Text for matching: full- and half-width forms alike, case ignored (the
 *  final sigma folded too, which lowercasing alone leaves apart). */
const fold = (text: string): string => text.normalize('NFKC').toLowerCase().replace(/ς/g, 'σ')
const haystackOf = (item: ChatHistorySummary): string => fold(`${item.title}\n${item.first_prompt}`)

const clipped = (text: string, chars: number): string => {
  const g = graphemes(text)
  return g.length > chars ? `${g.slice(0, chars).join('')}…` : text
}

/** UI-03: the main chat's recent conversations, listed in the sidebar under
 *  the assistant. Clicking one shows the chat page with that conversation;
 *  each can be renamed (UI-04: ✎, a double click or F2) and deleted, and all
 *  can be cleared after a confirmation. */
export function ChatHistoryList({ showChat }: { showChat: () => void }) {
  const t = useT(chatMessages)
  const { items, activeId, busy } = useSyncExternalStore(subscribeHistory, getHistoryState)
  useEffect(() => { void refreshHistory() }, [])
  const [query, setQuery] = useState('')
  const searchRef = useRef<HTMLInputElement>(null)
  // What each row shows, worked out once per list, not on every keystroke
  // of a rename or a search. (`t` is new on every render; its text is not.)
  const untitled = t('newChat')
  const rows = useMemo(() => {
    // Titles shared by more than one conversation get their time beside them.
    const titleCount = new Map<string, number>()
    for (const item of items) {
      const name = item.title || untitled
      titleCount.set(name, (titleCount.get(name) ?? 0) + 1)
    }
    return items.map(item => {
      const name = item.title || untitled
      const when = shortTime(item.updated_at)
      const twin = (titleCount.get(name) ?? 0) > 1 && when !== ''
      return {
        item, name, when, twin,
        // Buttons of conversations with the same title say which is which.
        title: twin ? `${name} ${when}` : name,
        tip: [name, item.first_prompt && item.first_prompt !== name ? clipped(item.first_prompt, 80) : '', when]
          .filter(Boolean).join(' · '),
        haystack: haystackOf(item),
      }
    })
  }, [items, untitled])
  // The search, and so its filter, only while the list is long enough to
  // have one.
  const searchable = items.length > SEARCH_FROM
  const needle = fold(oneLine(query))
  const searching = searchable && needle !== ''
  const shown = searching ? rows.filter(row => row.haystack.includes(needle)) : rows
  // The conversation being renamed, the title it had when the box opened,
  // and what has been typed so far. The ref makes Enter followed by the
  // input's blur rename once, not twice.
  const [editing, setEditing] = useState<string | null>(null)
  const [draft, setDraft] = useState('')
  const editingRef = useRef<{ id: string; from: string } | null>(null)
  const listRef = useRef<HTMLUListElement>(null)
  const startEdit = (id: string, title: string) => {
    editingRef.current = { id, from: title }
    setEditing(id)
    setDraft(title)
  }
  const endEdit = (save: boolean, refocus: boolean) => {
    const edit = editingRef.current
    if (edit === null) return
    const { id, from } = edit
    editingRef.current = null
    setEditing(null)
    // Only what the user changed: a box left as it opened renames nothing,
    // even if the title changed meanwhile (an earlier rename landing). Compared
    // before the cut to 80 characters, so shortening a longer title counts.
    const title = renameTitle(draft)
    const renamed = save && title && oneLine(draft) !== oneLine(from) ? renameConversation(id, title) : null
    // From the keyboard, focus goes back to the conversation's entry. When
    // the new title takes the row out of the results and focus went with it
    // (it was on the row, or on its buttons after Tab), it moves to the
    // search box.
    const entry = () => listRef.current?.querySelector<HTMLButtonElement>(`[data-history-id="${id}"]`)
    if (refocus) requestAnimationFrame(() => entry()?.focus())
    void renamed?.then(() => requestAnimationFrame(() => {
      const lost = document.activeElement === null || document.activeElement === document.body
      if (lost && !entry()) searchRef.current?.focus()
    }))
  }
  if (items.length === 0) return null
  return (
    <section className="chat-history" aria-label={t('history.label')}>
      <div className="chat-history-head">
        <span>{t('history.recent')}</span>
        <button type="button" className="chat-history-clear" disabled={busy} onClick={() => {
          if (window.confirm(t('history.clearConfirm'))) void clearConversations()
        }}>{t('history.clear')}</button>
      </div>
      {searchable && (
        <input type="search" className="chat-history-search" aria-label={t('history.search')} ref={searchRef}
          placeholder={t('history.search')} value={query} onChange={e => setQuery(e.target.value)}
          onKeyDown={e => {
            // Escape during an input method's composition drops the candidate.
            if (e.nativeEvent.isComposing || e.keyCode === 229) return
            if (e.key === 'Escape' && query !== '') { e.preventDefault(); e.stopPropagation(); setQuery('') }
          }} />
      )}
      {searching && shown.length === 0 && <p className="chat-history-empty" role="status">{t('history.noMatch')}</p>}
      <ul className="chat-history-list" ref={listRef}>
        {shown.map(({ item, name, when, twin, title, tip }) => {
          return (
            <li key={item.id} className={`chat-history-item${item.id === activeId ? ' active' : ''}`}>
              {editing === item.id
                ? <input className="chat-history-rename" aria-label={t('history.renameInput')} value={draft}
                  autoFocus onFocus={e => e.currentTarget.select()}
                  onChange={e => setDraft(e.target.value)}
                  onKeyDown={e => {
                    // Enter and Escape during an input method's composition
                    // pick or drop a candidate; they are not ours.
                    if (e.nativeEvent.isComposing || e.keyCode === 229) return
                    if (e.key === 'Enter') { e.preventDefault(); endEdit(true, true) }
                    else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); endEdit(false, true) }
                  }}
                  onBlur={() => endEdit(true, false)} />
                : <button type="button" className="chat-history-open" title={tip} data-history-id={item.id}
                  disabled={busy && item.id !== activeId}
                  aria-current={item.id === activeId ? 'true' : undefined}
                  onClick={() => { openConversation(item.id); showChat() }}
                  onDoubleClick={() => startEdit(item.id, item.title)}
                  onKeyDown={e => { if (e.key === 'F2') { e.preventDefault(); startEdit(item.id, item.title) } }}>
                  <span className="chat-history-name">{name}</span>
                  {twin && <>{' '}<span className="chat-history-when">{when}</span></>}
                </button>}
              {editing !== item.id && (
                <button type="button" className="chat-history-action" aria-label={t('history.rename', { title })}
                  onClick={() => startEdit(item.id, item.title)}>✎</button>
              )}
              <button type="button" className="chat-history-action chat-history-delete" aria-label={t('history.delete', { title })}
                disabled={busy && item.id === activeId}
                onClick={() => void deleteConversation(item.id)}>×</button>
            </li>
          )
        })}
      </ul>
    </section>
  )
}
