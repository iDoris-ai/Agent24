import { useEffect, useSyncExternalStore } from 'react'
import { useT } from '../../i18n'
import { chatMessages } from '../Chat.i18n'
import {
  clearConversations,
  deleteConversation,
  getHistoryState,
  openConversation,
  refreshHistory,
  subscribeHistory,
} from './history'

/** UI-03: the main chat's recent conversations, listed in the sidebar under
 *  the assistant. Clicking one shows the chat page with that conversation;
 *  each can be deleted, and all can be cleared after a confirmation. */
export function ChatHistoryList({ showChat }: { showChat: () => void }) {
  const t = useT(chatMessages)
  const { items, activeId, busy } = useSyncExternalStore(subscribeHistory, getHistoryState)
  useEffect(() => { void refreshHistory() }, [])
  if (items.length === 0) return null
  return (
    <section className="chat-history" aria-label={t('history.label')}>
      <div className="chat-history-head">
        <span>{t('history.recent')}</span>
        <button type="button" className="chat-history-clear" disabled={busy} onClick={() => {
          if (window.confirm(t('history.clearConfirm'))) void clearConversations()
        }}>{t('history.clear')}</button>
      </div>
      <ul className="chat-history-list">
        {items.map(item => {
          const title = item.title || t('newChat')
          return (
            <li key={item.id} className={`chat-history-item${item.id === activeId ? ' active' : ''}`}>
              <button type="button" className="chat-history-open" title={title} disabled={busy && item.id !== activeId}
                aria-current={item.id === activeId ? 'true' : undefined}
                onClick={() => { openConversation(item.id); showChat() }}>{title}</button>
              <button type="button" className="chat-history-delete" aria-label={t('history.delete', { title })}
                disabled={busy && item.id === activeId}
                onClick={() => void deleteConversation(item.id)}>×</button>
            </li>
          )
        })}
      </ul>
    </section>
  )
}
