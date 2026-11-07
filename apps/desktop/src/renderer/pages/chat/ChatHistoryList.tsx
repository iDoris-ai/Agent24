import { useEffect, useRef, useState, useSyncExternalStore } from 'react'
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

const fold = (text: string): string => text.normalize('NFKC').toLowerCase()
/** A conversation matches when its title or first prompt contains the query,
 *  ignoring case and full- or half-width forms. */
export const matchesQuery = (item: ChatHistorySummary, query: string): boolean =>
  fold(`${item.title}\n${item.first_prompt}`).includes(fold(oneLine(query)))

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
  // The search, and so its filter, only while the list is long enough to
  // have one.
  const searchable = items.length > SEARCH_FROM
  const searching = searchable && oneLine(query) !== ''
  const shown = searching ? items.filter(item => matchesQuery(item, query)) : items
  // Titles shared by more than one conversation get their time beside them.
  const titleCount = new Map<string, number>()
  for (const item of items) {
    const title = item.title || t('newChat')
    titleCount.set(title, (titleCount.get(title) ?? 0) + 1)
  }
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
    if (save && title && oneLine(draft) !== oneLine(from)) void renameConversation(id, title)
    // From the keyboard, focus goes back to the conversation's entry.
    if (refocus) {
      requestAnimationFrame(() => {
        listRef.current?.querySelector<HTMLButtonElement>(`[data-history-id="${id}"]`)?.focus()
      })
    }
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
        <input type="search" className="chat-history-search" aria-label={t('history.search')}
          placeholder={t('history.search')} value={query} onChange={e => setQuery(e.target.value)}
          onKeyDown={e => {
            if (e.key === 'Escape' && query !== '') { e.preventDefault(); e.stopPropagation(); setQuery('') }
          }} />
      )}
      {searching && shown.length === 0 && <p className="chat-history-empty" role="status">{t('history.noMatch')}</p>}
      <ul className="chat-history-list" ref={listRef}>
        {shown.map(item => {
          const name = item.title || t('newChat')
          const when = shortTime(item.updated_at)
          const twin = (titleCount.get(name) ?? 0) > 1
          // Buttons of conversations with the same title say which is which.
          const title = twin && when ? `${name} ${when}` : name
          const tip = [name, item.first_prompt && item.first_prompt !== name ? clipped(item.first_prompt, 80) : '', when]
            .filter(Boolean).join(' · ')
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
                  {twin && when && <>{' '}<span className="chat-history-when">{when}</span></>}
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
