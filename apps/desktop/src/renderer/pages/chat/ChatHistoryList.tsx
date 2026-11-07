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
import { RENAME_TITLE_CHARS, renameTitle } from './title'

/** UI-03: the main chat's recent conversations, listed in the sidebar under
 *  the assistant. Clicking one shows the chat page with that conversation;
 *  each can be renamed (UI-04: ✎, a double click or F2) and deleted, and all
 *  can be cleared after a confirmation. */
export function ChatHistoryList({ showChat }: { showChat: () => void }) {
  const t = useT(chatMessages)
  const { items, activeId, busy } = useSyncExternalStore(subscribeHistory, getHistoryState)
  useEffect(() => { void refreshHistory() }, [])
  // The conversation being renamed and what has been typed so far. The ref
  // makes Enter followed by the input's blur rename once, not twice.
  const [editing, setEditing] = useState<string | null>(null)
  const [draft, setDraft] = useState('')
  const editingRef = useRef<string | null>(null)
  const listRef = useRef<HTMLUListElement>(null)
  const startEdit = (id: string, title: string) => {
    editingRef.current = id
    setEditing(id)
    setDraft(title)
  }
  const endEdit = (save: boolean, refocus: boolean) => {
    const id = editingRef.current
    if (id === null) return
    editingRef.current = null
    setEditing(null)
    const current = items.find(item => item.id === id)?.title
    const title = renameTitle(draft)
    if (save && title && title !== current) void renameConversation(id, title)
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
      <ul className="chat-history-list" ref={listRef}>
        {items.map(item => {
          const title = item.title || t('newChat')
          return (
            <li key={item.id} className={`chat-history-item${item.id === activeId ? ' active' : ''}`}>
              {editing === item.id
                ? <input className="chat-history-rename" aria-label={t('history.renameInput')} value={draft}
                  maxLength={RENAME_TITLE_CHARS * 4} autoFocus onFocus={e => e.currentTarget.select()}
                  onChange={e => setDraft(e.target.value)}
                  onKeyDown={e => {
                    if (e.key === 'Enter') { e.preventDefault(); endEdit(true, true) }
                    else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); endEdit(false, true) }
                  }}
                  onBlur={() => endEdit(true, false)} />
                : <button type="button" className="chat-history-open" title={title} data-history-id={item.id}
                  disabled={busy && item.id !== activeId}
                  aria-current={item.id === activeId ? 'true' : undefined}
                  onClick={() => { openConversation(item.id); showChat() }}
                  onDoubleClick={() => startEdit(item.id, item.title)}
                  onKeyDown={e => { if (e.key === 'F2') { e.preventDefault(); startEdit(item.id, item.title) } }}>{title}</button>}
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
