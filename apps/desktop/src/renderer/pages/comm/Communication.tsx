import { useCallback, useEffect, useRef, useState } from 'react'
import {
  addContact,
  getCommStatus,
  listCommContacts,
  listCommIdentities,
  listCommRelays,
  startCommDaemon,
  stopCommDaemon,
  unlockComm,
  type CommContact,
  type CommDaemonStatus,
  type CommIdentity,
  type CommRelayConfig,
} from './api'
import './communication.css'

type WriteAction = 'start' | 'stop' | 'unlock' | 'addContact'

// Bech32 charset (BIP-173). Structural-only mirror of agent24-comm's
// `is_valid_npub` — hrp, length, and charset, WITHOUT verifying the
// checksum. The server remains the sole source of truth; this only keeps an
// obviously malformed value from reaching a write request.
const BECH32_CHARSET = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l'

function looksLikeNpub(value: string): boolean {
  // hrp === 'npub' and every data char in BECH32_CHARSET already forces
  // ASCII lowercase, so no separate ASCII/case check is needed here.
  if (value.length < 8 || value.length > 90) return false
  const sep = value.lastIndexOf('1')
  if (sep <= 0 || value.length - sep - 1 < 6) return false
  if (value.slice(0, sep) !== 'npub') return false
  const data = value.slice(sep + 1)
  return data.split('').every((char) => BECH32_CHARSET.includes(char))
}

function messageOf(error: unknown, secret = ''): string {
  const text = error instanceof Error ? error.message : String(error)
  return secret ? text.split(secret).join('•••') : text
}

function processLabel(state: string): string {
  const labels: Record<string, string> = {
    stopped: '已停止', starting: '正在启动', running: '运行中', backoff: '退避重试',
    gave_up: '已停止重试', locked: '已锁定', not_configured: '未配置',
  }
  return labels[state] ?? state
}

function reasonLabel(reason: string | null | undefined): string {
  if (!reason) return ''
  const labels: Record<string, string> = {
    no_identity: '尚无默认身份', no_relay: '尚未配置 relay',
    keychain_unavailable: '系统钥匙串不可用', password_rejected: '口令被拒绝',
    binary_rejected: 'Hyphae 二进制校验失败', misconfigured: '配置有误',
    not_configured: '通信尚未配置', locked: '通信服务已锁定',
  }
  return labels[reason] ?? '其他原因'
}

function DaemonStatusView({ status, fresh }: { status: CommDaemonStatus | null; fresh: boolean }) {
  if (!status) return <p className="comm-muted">尚未载入状态</p>
  const process = status.process
  const probe = status.relay_probe
  const catchUp = status.catch_up
  return (
    <div className="comm-status-grid">
      <section className="comm-status-item">
        <h3>进程</h3>
        <p><span className={`comm-state${fresh && process.state === 'running' ? ' comm-state-running' : ''}`}>{processLabel(process.state)}</span></p>
        {process.reason && <p className="comm-muted">原因 {process.reason}：{reasonLabel(process.reason)}</p>}
        <p className="comm-muted">配置代次 {process.generation} · 连续失败 {process.consecutive_failures}</p>
      </section>
      <section className="comm-status-item">
        <h3>Relay 探测</h3>
        {probe ? <>
          <p>{probe.connected ? '最近一次握手成功' : '最近一次握手失败'}</p>
          {probe.url && <p className="comm-muted">{probe.url}</p>}
          {probe.at_ms !== null && <p className="comm-muted">时间戳 {probe.at_ms}</p>}
          {probe.error && <p className="comm-muted">{probe.error}</p>}
        </> : <p className="comm-muted">尚无手动探测结果</p>}
      </section>
      <section className="comm-status-item">
        <h3>补收</h3>
        <p>{catchUp.state === 'incomplete' ? '检测到补收不完整' : '状态未知'}</p>
        {catchUp.last_incomplete_at_ms !== null && <p className="comm-muted">最近不完整时间戳 {catchUp.last_incomplete_at_ms}</p>}
      </section>
    </div>
  )
}

export default function CommunicationPage() {
  const [identities, setIdentities] = useState<CommIdentity[] | null>(null)
  const [contacts, setContacts] = useState<CommContact[] | null>(null)
  const [relays, setRelays] = useState<CommRelayConfig | null>(null)
  const [daemon, setDaemon] = useState<CommDaemonStatus | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [refreshing, setRefreshing] = useState(false)
  const [pending, setPending] = useState<WriteAction | null>(null)
  const [password, setPassword] = useState('')
  const [remember, setRemember] = useState(false)
  const [actionError, setActionError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [contactNickname, setContactNickname] = useState('')
  const [contactNpub, setContactNpub] = useState('')
  const [contactRole, setContactRole] = useState('')
  const sequence = useRef(0)
  const mounted = useRef(false)

  const refresh = useCallback(async () => {
    const token = ++sequence.current
    setRefreshing(true)
    setLoadError(null)
    const results = await Promise.allSettled([
      listCommIdentities(), listCommContacts(), listCommRelays(), getCommStatus(),
    ])
    if (!mounted.current || token !== sequence.current) return
    const failures: string[] = []
    if (results[0].status === 'fulfilled') setIdentities(results[0].value)
    else failures.push(`身份：${messageOf(results[0].reason)}`)
    if (results[1].status === 'fulfilled') setContacts(results[1].value)
    else failures.push(`联系人：${messageOf(results[1].reason)}`)
    if (results[2].status === 'fulfilled') setRelays(results[2].value)
    else failures.push(`Relay：${messageOf(results[2].reason)}`)
    if (results[3].status === 'fulfilled') setDaemon(results[3].value)
    else failures.push(`进程状态：${messageOf(results[3].reason)}`)
    setLoadError(failures.length ? `刷新失败，已保留上次数据（可能已过期）：${failures.join('；')}` : null)
    setRefreshing(false)
  }, [])

  useEffect(() => {
    mounted.current = true
    void refresh()
    return () => { mounted.current = false; sequence.current++ }
  }, [refresh])

  const runAction = async (action: WriteAction) => {
    if (pending) return
    const token = ++sequence.current // invalidate a read already in flight
    setPending(action)
    setActionError(null)
    setNotice(null)
    const submittedPassword = action === 'unlock' ? password : ''
    if (action === 'unlock') setPassword('')
    try {
      if (action === 'start') {
        await startCommDaemon()
        if (mounted.current && token === sequence.current) setNotice('已提交启动请求。')
      } else if (action === 'stop') {
        await stopCommDaemon()
        if (mounted.current && token === sequence.current) setNotice('已提交停止请求。')
      } else if (action === 'unlock') {
        const result = await unlockComm(submittedPassword, remember)
        if (mounted.current && token === sequence.current) {
          setNotice(result.remembered ? '已解锁，口令已记入系统钥匙串。' : '已解锁，本次会话内有效。')
        }
      } else {
        await addContact(contactNickname, contactNpub, contactRole.trim() ? contactRole : undefined)
        if (mounted.current && token === sequence.current) {
          setNotice('已添加联系人。')
          setContactNickname('')
          setContactNpub('')
          setContactRole('')
        }
      }
    } catch (error) {
      if (mounted.current && token === sequence.current) {
        const code = typeof error === 'object' && error !== null && 'code' in error
          ? (error as { code?: unknown }).code : undefined
        const knownCodes = ['binary_rejected', 'not_configured', 'locked', 'password_rejected', 'keychain_unavailable']
        const reason = typeof code === 'string' && knownCodes.includes(code) ? `${code}（${reasonLabel(code)}）: ` : ''
        setActionError(`${reason}${messageOf(error, submittedPassword)}`)
      }
    } finally {
      if (mounted.current) {
        if (action === 'unlock') setPassword('')
        setPending(null)
        void refresh()
      }
    }
  }

  const isFresh = !loadError && !refreshing && daemon !== null
  return (
    <main className="content comm-page">
      <header className="comm-heading">
        <div><h1>通信概览</h1><p>身份、联系人、Relay 与 Hyphae 服务状态</p></div>
        <button type="button" onClick={() => void refresh()} disabled={refreshing || pending !== null}>
          {refreshing ? '刷新中…' : '刷新'}
        </button>
      </header>

      {loadError && <div role="alert" className="comm-alert">{loadError}</div>}
      {actionError && <div role="alert" className="comm-alert">操作失败：{actionError}</div>}
      {notice && <div role="status" className="comm-notice">{notice}</div>}

      <section className="comm-card">
        <h2>身份</h2>
        {identities === null ? <p className="comm-muted">尚无可显示的数据</p> : identities.length === 0 ? <p className="comm-muted">暂无身份</p> : (
          <div className="comm-table-wrap"><table><thead><tr><th>昵称</th><th>公钥</th><th>默认身份</th><th>Keystore</th></tr></thead><tbody>
            {identities.map((identity) => <tr key={identity.nickname}>
              <td>{identity.nickname}</td><td className="comm-key">{identity.npub}</td>
              <td>{identity.default ? '默认身份' : '—'}</td><td>{identity.encrypted ? '已加密' : '未加密'}</td>
            </tr>)}
          </tbody></table></div>
        )}
      </section>

      <section className="comm-card">
        <h2>联系人</h2>
        {contacts === null ? <p className="comm-muted">尚无可显示的数据</p> : contacts.length === 0 ? <p className="comm-muted">暂无联系人</p> : (
          <div className="comm-table-wrap"><table><thead><tr><th>昵称</th><th>公钥</th><th>角色</th></tr></thead><tbody>
            {contacts.map((contact) => <tr key={`${contact.nickname}:${contact.npub}`}>
              <td>{contact.nickname}</td><td className="comm-key">{contact.npub}</td><td>{contact.role || '—'}</td>
            </tr>)}
          </tbody></table></div>
        )}
        <form
          className="comm-contact-form"
          onSubmit={(event) => {
            event.preventDefault()
            if (contactNickname.trim().length === 0 || !looksLikeNpub(contactNpub)) return
            void runAction('addContact')
          }}
        >
          <label>昵称<input type="text" value={contactNickname} onChange={(event) => setContactNickname(event.target.value)} /></label>
          <label>公钥（npub）<input type="text" value={contactNpub} onChange={(event) => setContactNpub(event.target.value)} /></label>
          <label>角色（可选）<input type="text" value={contactRole} onChange={(event) => setContactRole(event.target.value)} /></label>
          <button
            type="submit"
            disabled={pending !== null || contactNickname.trim().length === 0 || !looksLikeNpub(contactNpub)}
          >
            {pending === 'addContact' ? '添加中…' : '添加联系人'}
          </button>
        </form>
      </section>

      <section className="comm-card">
        <h2>Relay</h2>
        {relays === null ? <p className="comm-muted">尚无可显示的数据</p> : <>
          <p>来源：{relays.source} · {relays.configured ? '已配置' : '未配置'}</p>
          {relays.relays.length ? <ul>{relays.relays.map((relay) => <li key={relay}>{relay}</li>)}</ul> : <p className="comm-muted">尚未配置 Relay</p>}
        </>}
      </section>

      <section className="comm-card">
        <div className="comm-section-heading"><h2>Hyphae 服务</h2><span className={isFresh && daemon?.process.state === 'running' ? 'comm-fresh-indicator' : 'comm-muted'}>{isFresh ? '数据已刷新' : '数据可能已过期'}</span></div>
        <DaemonStatusView status={daemon} fresh={isFresh} />
        <div className="comm-actions">
          <button type="button" onClick={() => void runAction('start')} disabled={pending !== null}>启动服务</button>
          <button type="button" onClick={() => void runAction('stop')} disabled={pending !== null}>停止服务</button>
        </div>
        <form className="comm-unlock" onSubmit={(event) => { event.preventDefault(); void runAction('unlock') }}>
          <label>Keystore 口令<input type="password" autoComplete="off" value={password} onChange={(event) => setPassword(event.target.value)} /></label>
          <label className="comm-checkbox"><input type="checkbox" checked={remember} onChange={(event) => setRemember(event.target.checked)} />记入系统钥匙串</label>
          <button type="submit" disabled={pending !== null || password.length === 0}>{pending === 'unlock' ? '解锁中…' : '解锁'}</button>
        </form>
      </section>
    </main>
  )
}
