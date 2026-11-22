import { useEffect, useRef, useState } from 'react'
import { useT } from '../../i18n'
import { communicationMessages } from './Communication.i18n'
import {
  CommApiError, acceptCommGroupInvite, createCommGroup, getCommGroupMessageStatus, listCommGroupInvites,
  listCommGroupMembers, listCommGroupMessages, retryCommGroupMessage, sendCommGroupMessage,
  type CommContact, type CommGroup, type CommGroupMember, type CommGroupMessage, type CommGroupReport,
  type CommHistoryItem, type CommIdentity,
} from './api'

export type ConversationSelection = { kind: 'peer'; id: string } | { kind: 'group'; id: string } | null
const shortNpub = (npub: string) => npub.length > 20 ? `${npub.slice(0, 12)}…${npub.slice(-6)}` : npub
const errorMessage = (error: unknown) => error instanceof Error ? error.message : String(error)
const timestamp = (at: string) => Date.parse(at) || 0
function plaintext(text: string): string {
  // Keep line breaks, but bound untrusted text and remove display controls.
  // eslint-disable-next-line no-control-regex
  return text.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/g, '').slice(0, 32768)
}

export function ConversationList({ contacts, groups, history, conversationHistory, identities, selection, search, onSelect }: {
  contacts: CommContact[]; groups: CommGroup[]; history: CommHistoryItem[]; conversationHistory: CommHistoryItem[] | null
  identities: CommIdentity[]; selection: ConversationSelection; search: string; onSelect: (selection: ConversationSelection) => void
}) {
  const t = useT(communicationMessages)
  const me = identities.find(item => item.default)
  const peers = new Map<string, CommHistoryItem[]>(contacts.map(contact => [contact.npub, []]))
  // A removed contact's open conversation stays listed (its history is kept).
  if (selection?.kind === 'peer' && !peers.has(selection.id)) peers.set(selection.id, [])
  for (const item of history) {
    const peer = item.direction === 'out' || item.from === me?.npub || item.from === me?.nickname ? item.to || item.from : item.from
    if (peer) peers.set(peer, [...(peers.get(peer) ?? []), item])
  }
  const rows = [...peers.entries()].map(([peer, items]) => {
    const source = selection?.kind === 'peer' && selection.id === peer ? [...items, ...(conversationHistory ?? [])] : items
    const last = source.reduce<CommHistoryItem | null>((a, b) => !a || timestamp(b.at) > timestamp(a.at) ? b : a, null)
    return { kind: 'peer' as const, id: peer, name: contacts.find(c => c.npub === peer)?.nickname ?? shortNpub(peer), at: last ? timestamp(last.at) : 0, preview: last?.content ?? '' }
  })
  const groupRows = groups.map(group => ({ kind: 'group' as const, id: group.group_id, name: group.name, at: group.updated_at * 1000, preview: group.state === 'pending' ? t('group.pending') : t('group.label') }))
  const sorted = [...rows, ...groupRows].filter(row => `${row.name} ${row.preview}`.toLowerCase().includes(search.toLowerCase())).sort((a, b) => b.at - a.at)
  return <>{sorted.map(row => <button type="button" key={`${row.kind}:${row.id}`} className={`comm-conversation${selection?.kind === row.kind && selection.id === row.id ? ' selected' : ''}`} onClick={() => onSelect({ kind: row.kind, id: row.id })}>
    <span className="comm-avatar">{row.name.slice(0, 1).toUpperCase()}</span>
    <span className="comm-conversation-copy"><strong><span>{row.name}{row.kind === 'group' && <span className="comm-state">{t('group.label')}</span>}</span><time>{row.at ? new Date(row.at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : ''}</time></strong><small>{row.preview}</small></span>
  </button>)}{sorted.length === 0 && <p className="comm-chat-empty">{t('chat.empty')}</p>}</>
}

export function CreateGroup({ contacts, onCreated, onClose }: { contacts: CommContact[]; onCreated: (id: string) => void; onClose: () => void }) {
  const t = useT(communicationMessages)
  const [name, setName] = useState('')
  const [invitees, setInvitees] = useState<string[]>([])
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const submit = async () => {
    if (busy || !name.trim() || !invitees.length) return
    setBusy(true); setError(null)
    try { const result = await createCommGroup(name.trim(), invitees); onCreated(result.group_id) }
    catch (error) { setError(errorMessage(error)) }
    finally { setBusy(false) }
  }
  return <form className="comm-card comm-group-create" aria-label={t('group.new')} onSubmit={event => { event.preventDefault(); void submit() }}>
    <label>{t('group.name')}<input autoFocus maxLength={256} value={name} onChange={event => setName(event.target.value)} /></label>
    <div className="comm-group-contacts">{contacts.map(contact => <label className="comm-checkbox" key={contact.npub}><input type="checkbox" checked={invitees.includes(contact.npub)} onChange={event => setInvitees(current => event.target.checked ? [...current, contact.npub] : current.filter(npub => npub !== contact.npub))} />{contact.nickname}</label>)}</div>
    <p className="comm-muted">{t('group.hint')}</p>
    {error && <p role="alert" className="comm-alert">{error}</p>}
    <div className="comm-row-actions"><button disabled={busy || !name.trim() || !invitees.length} type="submit">{t('group.create')}</button><button type="button" disabled={busy} onClick={onClose}>{t('group.cancel')}</button></div>
  </form>
}

type Delivery = 'sending' | 'sent' | 'failed'
// `stored`: Hyphae has recorded this logical id (a report came back, or a
// 502 partial), so a retry goes to `retry`; otherwise the send is repeated.
type Outgoing = { id: string; body: string; at: number; state: Delivery; stored: boolean }
/** At most this many status reads per refresh; each one is a Hyphae process. */
const STATUS_CONCURRENCY = 4
function delivery(report: CommGroupReport): Delivery {
  if (report.partial || report.failed > 0 || report.state === 'failed' || report.recipients.some(r => r.state === 'failed')) return 'failed'
  return report.queued === 0 && report.recipients.length > 0 && report.recipients.every(r => r.state === 'relay_accepted') ? 'sent' : 'sending'
}

export function GroupChat({ group, contacts, identities, refreshTick, onRefresh }: {
  group: CommGroup; contacts: CommContact[]; identities: CommIdentity[]; refreshTick: number; onRefresh: () => void
}) {
  const t = useT(communicationMessages)
  const [messages, setMessages] = useState<CommGroupMessage[]>([])
  const [members, setMembers] = useState<CommGroupMember[]>([])
  const [outgoing, setOutgoing] = useState<Outgoing[]>([])
  const outgoingRef = useRef(outgoing)
  outgoingRef.current = outgoing
  const [states, setStates] = useState<Record<string, Delivery>>({})
  const [body, setBody] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const streamRef = useRef<HTMLDivElement>(null)
  const atBottom = useRef(true)
  const me = identities.find(identity => identity.default)?.npub
  const displayName = (npub: string) => contacts.find(contact => contact.npub === npub)?.nickname || identities.find(identity => identity.npub === npub)?.nickname || shortNpub(npub)

  useEffect(() => {
    let current = true
    const load = async () => {
      try {
        if (group.state !== 'active') {
          const result = await listCommGroupMembers(group.group_id)
          if (current) { setMembers(result); setError(null) }
          return
        }
        const history = await listCommGroupMessages(group.group_id)
        if (!current) return
        setMessages(history); setError(null)
        // Only messages still in flight are polled; a settled one is never read again.
        const ids = outgoingRef.current.filter(message => message.state === 'sending' && message.stored).map(message => message.id)
        for (let i = 0; i < ids.length; i += STATUS_CONCURRENCY) {
          const results = await Promise.allSettled(ids.slice(i, i + STATUS_CONCURRENCY).map(async id => ({ id, report: await getCommGroupMessageStatus(group.group_id, id) })))
          if (!current) return
          // Applied per batch, so a reload that starts mid-way keeps what was read.
          for (const result of results) {
            if (result.status !== 'fulfilled') continue
            const { id, report } = result.value
            const state = delivery(report)
            setStates(previous => ({ ...previous, [id]: state }))
            setOutgoing(previous => previous.map(message => message.id === id && message.state !== 'failed' ? { ...message, state } : message))
          }
        }
      } catch (error) { if (current) setError(errorMessage(error)) }
    }
    void load()
    return () => { current = false }
  }, [group.group_id, group.state, refreshTick, me])

  useEffect(() => {
    if (atBottom.current && streamRef.current) streamRef.current.scrollTop = streamRef.current.scrollHeight
  }, [messages, outgoing])

  const join = async () => {
    setBusy(true); setError(null)
    try {
      const invites = await listCommGroupInvites(group.group_id)
      const invite = invites.find(item => item.group_id === group.group_id && item.invitee_npub === me && item.state === 'pending')
      if (!invite) throw new Error(t('group.noInvite'))
      await acceptCommGroupInvite(invite.invite_id)
      onRefresh()
    } catch (error) { setError(errorMessage(error)) }
    finally { setBusy(false) }
  }
  const send = async (retry?: Outgoing) => {
    const text = retry?.body ?? body.trim()
    if (busy || !text) return
    const id = retry?.id ?? crypto.randomUUID().replaceAll('-', '')
    const at = retry?.at ?? Date.now() / 1000
    setBusy(true); setError(null); if (!retry) setBody('')
    const stored = retry?.stored ?? false
    setOutgoing(previous => [...previous.filter(item => item.id !== id), { id, at, body: text, state: 'sending', stored }])
    try {
      const report = stored ? await retryCommGroupMessage(group.group_id, id) : await sendCommGroupMessage(group.group_id, text, id)
      const state = delivery(report)
      setOutgoing(previous => previous.map(item => item.id === id ? { ...item, state, stored: true } : item))
      setStates(previous => ({ ...previous, [id]: state }))
      onRefresh()
    } catch (error) {
      // Only the daemon's `partial` (502) means Hyphae stored the message and some
      // recipients failed; a 502 `network` or `proxy_failed` proves nothing.
      const partial = error instanceof CommApiError && error.status === 502 && error.code === 'partial'
      setOutgoing(previous => previous.map(item => item.id === id ? { ...item, state: 'failed', stored: stored || partial } : item))
      setError(errorMessage(error))
    } finally { setBusy(false) }
  }
  const rows = [...messages.map(message => ({ id: message.id, body: message.plaintext, at: message.created_at, sender: message.sender })), ...outgoing.filter(item => !messages.some(message => message.id === item.id)).map(item => ({ ...item, sender: me ?? '' }))].sort((a, b) => a.at - b.at)
  return <>
    <div className="comm-message-stream" role="log" aria-label={group.name} ref={streamRef} onScroll={event => { const el = event.currentTarget; atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 100 }}>
      {error && <p role="alert" className="comm-alert">{error}</p>}
      {group.state !== 'active' ? <section className="comm-card">
        {group.creator_npub === me ? <><p>{t('group.progress')}</p>{members.map(member => <p key={member.npub}>{displayName(member.npub)} · {member.state === 'pending' ? t('group.waiting') : member.state === 'accepted' || member.state === 'active' ? t('group.accepted') : member.state}</p>)}</> : <><p>{t('group.invited', { name: displayName(group.creator_npub), group: group.name })}</p><button type="button" disabled={busy} onClick={() => void join()}>{t('group.join')}</button></>}
      </section> : rows.length ? rows.map(row => {
        const local = outgoing.find(item => item.id === row.id)
        const state = local?.state ?? states[row.id]
        const mine = row.sender === me
        return <div className={`comm-message-row${mine ? ' mine' : ''}`} key={row.id}><div className={`comm-message-bubble${mine && state === 'failed' ? ' failed' : ''}`}><strong>{displayName(row.sender)}</strong><div>{plaintext(row.body)}</div><small><time>{new Date(row.at * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}</time>{mine && state && <> · <span>{t(state === 'sent' ? 'group.sent' : state === 'failed' ? 'group.failed' : 'group.sending')}</span></>}</small>{mine && state === 'failed' && <button type="button" disabled={busy} onClick={() => void send(local ?? { id: row.id, body: row.body, at: row.at, state: 'failed', stored: true })}>{t('chat.retry')}</button>}</div></div>
      }) : <p className="comm-chat-empty">{t('chat.empty')}</p>}
    </div>
    {group.state === 'active' && <form className="comm-composer" onSubmit={event => { event.preventDefault(); void send() }}><textarea aria-label={t('chat.input')} placeholder={t('chat.input')} value={body} onChange={event => setBody(event.target.value)} onKeyDown={event => { if (event.nativeEvent.isComposing || event.keyCode === 229) return; if (event.key === 'Enter' && !event.shiftKey) { event.preventDefault(); void send() } }} /><button type="submit" disabled={busy || !body.trim()}>{t('message.send')}</button></form>}
  </>
}
