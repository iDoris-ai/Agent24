import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react'
import { useT } from '../../i18n'
import { chatMessages } from '../Chat.i18n'
import { parseSkillList, selectableSkillIds } from '../skills/model'
import {
  SKILLS_CHANGED_EVENT,
  setConversationSkill,
  skillForConversation,
  subscribeConversationSkill,
} from './session-skill'

/** Active skills only. The choice is kept with this conversation, not on the daemon. */
export function SkillPicker({ conversation, hasMessages, busy, onStartWithSkill }: {
  conversation: string
  /** The conversation already has turns: a skill cannot be attached to it. */
  hasMessages: boolean
  busy: boolean
  /** Opens a new conversation with this skill (or none) chosen. */
  onStartWithSkill: (skillId: string | null) => void
}) {
  const t = useT(chatMessages)
  const selected = useSyncExternalStore(
    subscribeConversationSkill,
    () => skillForConversation(conversation),
    () => null,
  )
  const [options, setOptions] = useState<string[] | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [failed, setFailed] = useState(false)
  const latest = useRef(0)
  const conversationRef = useRef(conversation)
  conversationRef.current = conversation

  // Never cached: every call asks again. A failure leaves the last good list.
  const load = useCallback(() => {
    const proxy = window.agent24?.backendProxy
    if (!proxy) return
    const ticket = ++latest.current
    void Promise.resolve(proxy({ method: 'GET', path: '/api/v1/skills' })).then((res) => {
      if (ticket !== latest.current) return
      if (!res.ok) { setFailed(true); return }
      const ids = selectableSkillIds(parseSkillList(res.data))
      setFailed(false)
      setOptions(ids)
      // A choice the list no longer has is dropped, so it is never sent unseen.
      const current = skillForConversation(conversationRef.current)
      if (current && !ids.includes(current)) {
        setConversationSkill(conversationRef.current, null)
        setNotice(current)
      }
    }, () => { if (ticket === latest.current) setFailed(true) })
  }, [])

  useEffect(() => {
    load()
    window.addEventListener(SKILLS_CHANGED_EVENT, load)
    window.addEventListener('focus', load)
    return () => {
      window.removeEventListener(SKILLS_CHANGED_EVENT, load)
      window.removeEventListener('focus', load)
    }
  }, [load])

  // Until the list is known, the chosen id still shows, so what is sent is what is seen.
  const ids = options ?? []
  const shown = selected && !ids.includes(selected) ? [...ids, selected] : ids

  return (
    <label className="chat-skill">
      {t('skill.label')}
      <select
        aria-label={t('skill.label')}
        value={selected ?? ''}
        onFocus={load}
        onMouseDown={load}
        disabled={hasMessages && busy}
        onChange={(event) => {
          setNotice(null)
          const next = event.target.value || null
          if (hasMessages) onStartWithSkill(next)
          else setConversationSkill(conversation, next)
        }}
      >
        <option value="">{t('skill.none')}</option>
        {shown.map((id) => <option key={id} value={id}>{id}</option>)}
      </select>
      {hasMessages && <span className="chat-skill-note">{t('skill.newChatHint')}</span>}
      {failed && <span role="alert" className="chat-skill-note">{t('skill.loadFailed')}</span>}
      {notice && <span role="status" className="chat-skill-note">{t('skill.stale', { name: notice })}</span>}
    </label>
  )
}
