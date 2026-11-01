// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { setLocale } from '../../i18n'
import CommunicationPage from './Communication'
import {
  getCommStatus, listCommContacts, listCommIdentities, listCommRelays,
  startCommDaemon, stopCommDaemon, unlockComm, installComm,
  addCommContact, sendCommMessage, listCommHistory, exportCommIdentity,
  CommApiError, type CommDaemonStatus,
} from './api'

vi.mock('./api', async (importActual) => ({
  CommApiError: (await importActual<typeof import('./api')>()).CommApiError,
  getCommStatus: vi.fn(), listCommContacts: vi.fn(), listCommIdentities: vi.fn(), listCommRelays: vi.fn(),
  startCommDaemon: vi.fn(), stopCommDaemon: vi.fn(), unlockComm: vi.fn(), installComm: vi.fn(),
  addCommContact: vi.fn(), sendCommMessage: vi.fn(), listCommHistory: vi.fn(), exportCommIdentity: vi.fn(),
}))

const identity = { nickname: 'alice', npub: 'npub1alice', default: true, encrypted: true }
const contact = { nickname: 'bob', npub: 'npub1bob', role: 'friend' }
const relayConfig = { relays: ['wss://relay.example'], source: 'config', configured: true }
const status: CommDaemonStatus = {
  process: { state: 'running', generation: 2, consecutive_failures: 0, reason: null },
  relay_probe: { url: 'wss://relay.example', connected: true, at_ms: 100, error: null },
  catch_up: { state: 'unknown' as const, last_incomplete_at_ms: null },
}

function success() {
  vi.mocked(listCommIdentities).mockResolvedValue([identity])
  vi.mocked(listCommContacts).mockResolvedValue([contact])
  vi.mocked(listCommRelays).mockResolvedValue(relayConfig)
  vi.mocked(getCommStatus).mockResolvedValue(status)
  vi.mocked(startCommDaemon).mockResolvedValue(status)
  vi.mocked(stopCommDaemon).mockResolvedValue({ ...status, process: { ...status.process, state: 'stopped' } })
  vi.mocked(unlockComm).mockResolvedValue({ unlocked: true, remembered: false })
  vi.mocked(listCommHistory).mockResolvedValue([])
}
const lockedStatus: CommDaemonStatus = { ...status, process: { ...status.process, state: 'locked', reason: 'password_rejected' } }

afterEach(() => { cleanup(); vi.clearAllMocks() })

describe('CommunicationPage', () => {
  beforeEach(() => { setLocale('zh') })
  it('offers one-click download when the binary is missing', async () => {
    success()
    for (const fn of [listCommIdentities, listCommContacts, listCommRelays]) {
      vi.mocked(fn).mockRejectedValue(new CommApiError('no Hyphae binary is configured', 409, 'not_configured'))
    }
    vi.mocked(getCommStatus).mockResolvedValue({ ...status, installable: true, installed_version: null })
    render(<CommunicationPage />)
    vi.mocked(installComm).mockImplementation(async () => {
      success()
      return { installed_version: 'v0.27.2', path: '/state/components/hyphae/v0.27.2/hyphae', restart_required: false, reason: null }
    })
    fireEvent.click(await screen.findByRole('button', { name: '下载并设置' }))
    expect(await screen.findByText('npub1alice')).toBeInTheDocument()
    expect(installComm).toHaveBeenCalledTimes(1)
    expect(screen.getByText('Hyphae 已就绪，通信状态已刷新。')).toBeInTheDocument()
  })

  it('renders identities, contacts, relay source, and independent status dimensions', async () => {
    success()
    render(<CommunicationPage />)
    expect(await screen.findByText('npub1alice')).toBeInTheDocument()
    expect(screen.getAllByText('默认身份')).toHaveLength(2)
    expect(screen.getByText('npub1bob')).toBeInTheDocument()
    expect(screen.getByRole('option', { name: 'wss://relay.example' })).toBeInTheDocument()
    expect(screen.getByRole('combobox', { name: 'Relay 服务器' })).toHaveValue('wss://relay.example')
    expect(screen.getByText('来源：config · 已配置')).toBeInTheDocument()
    expect(screen.getByText('运行中')).toBeInTheDocument()
    expect(screen.getByText('最近一次握手成功')).toBeInTheDocument()
    expect(screen.getByText('状态未知')).toBeInTheDocument()
    expect(screen.queryByText(/补收完成|已送达/)).not.toBeInTheDocument()
    expect(startCommDaemon).not.toHaveBeenCalled()
    expect(stopCommDaemon).not.toHaveBeenCalled()
  })

  it('not_configured on every read: shows the guide as a neutral status, not a Refresh-failed alert', async () => {
    const nc = () => new CommApiError('Not configured', 409, 'not_configured')
    vi.mocked(listCommIdentities).mockRejectedValue(nc())
    vi.mocked(listCommContacts).mockRejectedValue(nc())
    vi.mocked(listCommRelays).mockRejectedValue(nc())
    vi.mocked(getCommStatus).mockRejectedValue(nc())
    render(<CommunicationPage />)
    expect(await screen.findByText('通信尚未配置')).toBeInTheDocument()
    expect(screen.getByText('agent24 comm identity create <昵称> --default')).toBeInTheDocument()
    expect(screen.queryByRole('alert')).toBeNull()
    expect(screen.queryByText(/刷新失败|Refresh failed|not_configured/)).toBeNull()
  })

  it('not_configured because no Hyphae binary: binary guidance (not the identity command) plus the daemon detail', async () => {
    const nc = () => new CommApiError('no Hyphae binary is configured; set A24_HYPHAE_BIN (COMM-HYPHAE.md §3)', 409, 'not_configured')
    for (const fn of [listCommIdentities, listCommContacts, listCommRelays, getCommStatus]) vi.mocked(fn).mockRejectedValue(nc())
    render(<CommunicationPage />)
    expect(await screen.findByText('下载并设置 Hyphae，即可在这台电脑上使用通信功能。')).toBeInTheDocument()
    expect(screen.getByText('当前平台或 Agent24 版本暂不支持下载 Hyphae。')).toBeInTheDocument()
    expect(screen.getByText(/详情：no Hyphae binary is configured/)).toBeInTheDocument()
    expect(screen.queryByText('agent24 comm identity create <昵称> --default')).toBeNull()
    expect(screen.queryByRole('alert')).toBeNull()
  })

  it.each([
    'A24_COMM_PASSWORD_STORE="x" is not a recognized password store backend',
    'not a recognized relay configuration',
    'invalid configuration: relay URL must use wss',
    'misconfigured password store',
  ])('配置错误展示原文且不引导创建身份：%s', async (message) => {
    for (const fn of [listCommIdentities, listCommContacts, listCommRelays, getCommStatus]) {
      vi.mocked(fn).mockRejectedValue(new CommApiError(message, 409, 'not_configured'))
    }
    render(<CommunicationPage />)
    expect(await screen.findByRole('heading', { name: '通信配置有误' })).toBeInTheDocument()
    expect(screen.getByText(`详情：${message}`)).toBeInTheDocument()
    expect(screen.queryByText('agent24 comm identity create <昵称> --default')).toBeNull()
    expect(screen.queryByText('A24_HYPHAE_BIN=/path/to/hyphae')).toBeNull()
  })

  it('a real error still shows as an alert, listing only the real failure, next to the not_configured guide', async () => {
    const nc = () => new CommApiError('Not configured', 409, 'not_configured')
    vi.mocked(listCommIdentities).mockRejectedValue(nc())
    vi.mocked(listCommContacts).mockRejectedValue(nc())
    vi.mocked(listCommRelays).mockRejectedValue(new CommApiError('relay exploded', 500, 'internal'))
    vi.mocked(getCommStatus).mockRejectedValue(nc())
    render(<CommunicationPage />)
    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent('relay exploded')
    expect(alert).not.toHaveTextContent('身份：')
    expect(screen.getByText('通信尚未配置')).toBeInTheDocument()
  })

  it('all reads failing with a real error is shown as an error, with no setup guide', async () => {
    for (const fn of [listCommIdentities, listCommContacts, listCommRelays, getCommStatus]) {
      vi.mocked(fn).mockRejectedValue(new CommApiError('daemon down', 503, 'unavailable'))
    }
    render(<CommunicationPage />)
    expect(await screen.findByRole('alert')).toHaveTextContent('daemon down')
    expect(screen.queryByText('通信尚未配置')).toBeNull()
  })

  it('does not paint locked, gave_up, or missing relay as healthy', async () => {
    success()
    vi.mocked(listCommRelays).mockResolvedValue({ relays: [], source: 'default', configured: false })
    vi.mocked(getCommStatus).mockResolvedValue({ ...status, process: { ...status.process, state: 'locked', reason: 'binary_rejected' } })
    const { rerender } = render(<CommunicationPage />)
    expect(await screen.findByText('已锁定')).toBeInTheDocument()
    expect(screen.getByText('原因 binary_rejected：Hyphae 二进制校验失败')).toBeInTheDocument()
    expect(screen.getByText('来源：default · 未配置')).toBeInTheDocument()
    expect(screen.getByText('已锁定').className).not.toContain('comm-state-running')
    vi.mocked(getCommStatus).mockResolvedValue({ ...status, process: { ...status.process, state: 'gave_up', reason: 'misconfigured' } })
    fireEvent.click(screen.getByRole('button', { name: '刷新' }))
    await waitFor(() => expect(screen.getByText('已停止重试')).toBeInTheDocument())
    rerender(<CommunicationPage />)
  })

  it('preserves loaded data and marks it potentially stale when refresh fails', async () => {
    success()
    render(<CommunicationPage />)
    expect(await screen.findByText('npub1alice')).toBeInTheDocument()
    vi.mocked(listCommContacts).mockRejectedValueOnce(new Error('locked'))
    fireEvent.click(screen.getByRole('button', { name: '刷新' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('刷新失败，已保留上次数据（可能已过期）')
    expect(screen.getByText('npub1alice')).toBeInTheDocument()
    expect(screen.getByText('数据可能已过期')).toBeInTheDocument()
  })

  it('ignores late results from an earlier refresh', async () => {
    success()
    let resolveOld!: (value: typeof status) => void
    vi.mocked(getCommStatus).mockReturnValueOnce(new Promise((resolve) => { resolveOld = resolve }))
    render(<CommunicationPage />)
    fireEvent.click(screen.getByRole('button', { name: '启动服务' }))
    await waitFor(() => expect(getCommStatus).toHaveBeenCalledTimes(2))
    await waitFor(() => expect(screen.getByText('运行中')).toBeInTheDocument())
    resolveOld({ ...status, process: { ...status.process, state: 'gave_up' } })
    await Promise.resolve()
    expect(screen.queryByText('已停止重试')).not.toBeInTheDocument()
  })

  it('does not apply refresh results after unmount', async () => {
    success()
    let resolvePending!: (value: typeof status) => void
    vi.mocked(getCommStatus).mockReturnValueOnce(new Promise((resolve) => { resolvePending = resolve }))
    const view = render(<CommunicationPage />)
    view.unmount()
    resolvePending({ ...status, process: { ...status.process, state: 'gave_up' } })
    await Promise.resolve()
    expect(screen.queryByText('已停止重试')).not.toBeInTheDocument()
  })

  it('clears unlock password on success and failure and defaults remember to false', async () => {
    success()
    vi.mocked(getCommStatus).mockResolvedValue(lockedStatus)
    render(<CommunicationPage />)
    const input = await screen.findByLabelText('Keystore 口令') as HTMLInputElement
    fireEvent.change(input, { target: { value: 'secret-pass' } })
    fireEvent.click(screen.getByRole('button', { name: '解锁' }))
    await waitFor(() => expect(unlockComm).toHaveBeenCalledWith('secret-pass', false))
    await waitFor(() => expect(input.value).toBe(''))
    vi.mocked(unlockComm).mockRejectedValueOnce(new Error('locked'))
    fireEvent.change(input, { target: { value: 'another-secret' } })
    fireEvent.click(screen.getByRole('button', { name: '解锁' }))
    await waitFor(() => expect(input.value).toBe(''))
    expect(screen.getByRole('alert')).not.toHaveTextContent('another-secret')
  })

  it('prevents duplicate daemon actions while the write is pending', async () => {
    success()
    let finish!: (value: typeof status) => void
    vi.mocked(startCommDaemon).mockReturnValue(new Promise((resolve) => { finish = resolve }))
    render(<CommunicationPage />)
    await screen.findByText('npub1alice')
    const button = screen.getByRole('button', { name: '启动服务' })
    fireEvent.click(button)
    fireEvent.click(button)
    expect(startCommDaemon).toHaveBeenCalledTimes(1)
    expect(button).toBeDisabled()
    finish(status)
    await waitFor(() => expect(button).toBeEnabled())
  })

  it('uses only IPC proxy wrappers and never invokes fetch or storage for secrets', async () => {
    success()
    vi.mocked(getCommStatus).mockResolvedValue(lockedStatus)
    const fetchSpy = vi.spyOn(window, 'fetch')
    const storageSet = vi.spyOn(Storage.prototype, 'setItem')
    render(<CommunicationPage />)
    await screen.findByText('npub1alice')
    fireEvent.change(await screen.findByLabelText('Keystore 口令'), { target: { value: 'do-not-store' } })
    fireEvent.click(screen.getByRole('button', { name: '解锁' }))
    await waitFor(() => expect(unlockComm).toHaveBeenCalled())
    expect(fetchSpy).not.toHaveBeenCalled()
    expect(storageSet).not.toHaveBeenCalled()
    expect(screen.queryByText('do-not-store')).not.toBeInTheDocument()
  })
})

describe('CommunicationPage English default', () => {
  it('shows the English page heading', async () => {
    success()
    render(<CommunicationPage />)
    expect(screen.getByRole('heading', { name: 'Communication overview' })).toBeInTheDocument()
  })
})


describe('CommunicationPage usability', () => {
  beforeEach(() => { setLocale('en') })
  afterEach(() => { vi.useRealTimers() })

  it('hides the unlock form in the normal state', async () => {
    success()
    render(<CommunicationPage />)
    await screen.findByText('npub1alice')
    expect(screen.queryByLabelText('Keystore password')).toBeNull()
    expect(screen.queryByRole('button', { name: 'Unlock' })).toBeNull()
  })

  it.each([
    ['locked state', { state: 'locked' as const, reason: null }],
    ['password_rejected', { state: 'stopped' as const, reason: 'password_rejected' }],
    ['keychain_unavailable', { state: 'gave_up' as const, reason: 'keychain_unavailable' }],
  ])('shows the unlock form with an explanation when %s', async (_name, process) => {
    success()
    vi.mocked(getCommStatus).mockResolvedValue({ ...status, process: { ...status.process, ...process } })
    render(<CommunicationPage />)
    expect(await screen.findByLabelText('Keystore password')).toBeInTheDocument()
    expect(screen.getByText(/generated and kept in the system keychain automatically/)).toBeInTheDocument()
  })

  it('says the password is incorrect instead of the service-locked wording', async () => {
    success()
    vi.mocked(getCommStatus).mockResolvedValue(lockedStatus)
    vi.mocked(unlockComm).mockRejectedValueOnce(new CommApiError('password verification failed', 423, 'locked'))
    render(<CommunicationPage />)
    fireEvent.change(await screen.findByLabelText('Keystore password'), { target: { value: 'guess' } })
    fireEvent.click(screen.getByRole('button', { name: 'Unlock' }))
    expect(await screen.findByText('The password is incorrect')).toBeInTheDocument()
    expect(screen.queryByText(/service is locked/)).toBeNull()
  })

  it('shows a spinner while starting, polls until running, and shows no spinner once running', async () => {
    success()
    vi.useFakeTimers()
    const starting: CommDaemonStatus = { ...status, process: { ...status.process, state: 'starting' } }
    vi.mocked(getCommStatus).mockResolvedValue(starting)
    render(<CommunicationPage />)
    await act(async () => { await vi.advanceTimersByTimeAsync(10) })
    expect(screen.getByTestId('comm-spinner')).toBeInTheDocument()
    expect(screen.getByText('Starting…')).toBeInTheDocument()
    vi.mocked(getCommStatus).mockResolvedValue(status)
    await act(async () => { await vi.advanceTimersByTimeAsync(1100) })
    expect(screen.getByText('Running')).toBeInTheDocument()
    expect(screen.queryByTestId('comm-spinner')).toBeNull()
    const calls = vi.mocked(getCommStatus).mock.calls.length
    await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
    expect(vi.mocked(getCommStatus).mock.calls.length).toBe(calls)
  })

  it('stops polling after about 30 seconds and when unmounted', async () => {
    success()
    vi.useFakeTimers()
    vi.mocked(getCommStatus).mockResolvedValue({ ...status, process: { ...status.process, state: 'backoff' } })
    const view = render(<CommunicationPage />)
    await act(async () => { await vi.advanceTimersByTimeAsync(10) })
    await act(async () => { await vi.advanceTimersByTimeAsync(40_000) })
    const calls = vi.mocked(getCommStatus).mock.calls.length
    expect(calls).toBeGreaterThanOrEqual(25)
    expect(calls).toBeLessThanOrEqual(32)
    await act(async () => { await vi.advanceTimersByTimeAsync(10_000) })
    expect(vi.mocked(getCommStatus).mock.calls.length).toBe(calls)
    view.unmount()
  })

  it('shows each npub with a Copy button and the hint for the default identity', async () => {
    success()
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true })
    render(<CommunicationPage />)
    fireEvent.click(await screen.findByRole('button', { name: 'Copy alice' }))
    await waitFor(() => expect(writeText).toHaveBeenCalledWith('npub1alice'))
    expect(screen.getByText('Send this address to a colleague so they can add you.')).toBeInTheDocument()
  })

  it('adds a contact after validating the npub prefix', async () => {
    success()
    vi.mocked(addCommContact).mockResolvedValue({})
    render(<CommunicationPage />)
    await screen.findByText('npub1alice')
    expect(screen.getByText(/1\. Exchange npub addresses/)).toBeInTheDocument()
    fireEvent.change(screen.getByLabelText('Nickname'), { target: { value: 'carol' } })
    fireEvent.change(screen.getByLabelText('npub address'), { target: { value: 'nsec1nope' } })
    fireEvent.click(screen.getByRole('button', { name: 'Add contact' }))
    expect(await screen.findByText('The address must start with npub1')).toBeInTheDocument()
    expect(addCommContact).not.toHaveBeenCalled()
    fireEvent.change(screen.getByLabelText('npub address'), { target: { value: 'npub1carol' } })
    fireEvent.change(screen.getByLabelText('Role (optional)'), { target: { value: 'colleague' } })
    fireEvent.click(screen.getByRole('button', { name: 'Add contact' }))
    await waitFor(() => expect(addCommContact).toHaveBeenCalledWith('carol', 'npub1carol', 'colleague'))
    await waitFor(() => expect(listCommContacts).toHaveBeenCalledTimes(2))
  })

  it('sends an encrypted message to a contact and reports relays; lists recent messages', async () => {
    success()
    vi.mocked(listCommHistory).mockResolvedValue([{ id: 'e1', from: 'bob', content: 'hello there', at: '10:00' }])
    vi.mocked(sendCommMessage).mockResolvedValue({ published_to: 2 })
    render(<CommunicationPage />)
    expect(await screen.findByText('hello there')).toBeInTheDocument()
    fireEvent.change(screen.getByLabelText('To'), { target: { value: 'bob' } })
    fireEvent.change(screen.getByLabelText('Message'), { target: { value: 'hi bob' } })
    fireEvent.click(screen.getByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sendCommMessage).toHaveBeenCalledWith('bob', 'hi bob'))
    expect(await screen.findByText('Published to 2 relay(s).')).toBeInTheDocument()
    await waitFor(() => expect(listCommHistory).toHaveBeenCalledTimes(2))
  })

  it('exports with the nickname only; the page has no password fields and a cancelled step is silent', async () => {
    success()
    vi.mocked(exportCommIdentity)
      .mockRejectedValueOnce(new CommApiError('cancelled', 499, 'cancelled'))
      .mockResolvedValueOnce({ nickname: 'alice', npub: 'npub1alice', path: '/safe/alice.ncryptsec' })
    render(<CommunicationPage />)
    const button = await screen.findByRole('button', { name: 'Export private key backup alice' })
    fireEvent.click(button)
    await waitFor(() => expect(exportCommIdentity).toHaveBeenCalledTimes(1))
    expect(exportCommIdentity).toHaveBeenCalledWith('alice')
    await waitFor(() => expect(button).toBeEnabled())
    expect(screen.queryByRole('alert')).toBeNull()
    expect(screen.queryByLabelText(/backup password/i)).toBeNull()
    expect(document.querySelector('input[type="password"]')).toBeNull()
    fireEvent.click(button)
    expect(await screen.findByText(/saved to \/safe\/alice\.ncryptsec/)).toBeInTheDocument()
    expect(document.body.textContent).not.toMatch(/nsec1/)
  })

  it('shows an existing-file refusal as an error', async () => {
    success()
    vi.mocked(exportCommIdentity).mockRejectedValueOnce(new CommApiError('A file with that name already exists', 409, 'target_exists'))
    render(<CommunicationPage />)
    fireEvent.click(await screen.findByRole('button', { name: 'Export private key backup alice' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('already exists')
  })

  const stranger = 'npub1strangerstrangerstrangerstrangerstrangerstrangerxyz'

  it('offers Add as contact for an unknown sender, prefilling the form, but not for a contact', async () => {
    success()
    vi.mocked(listCommHistory).mockResolvedValue([
      { id: 'a', from: stranger, content: 'who dis', at: '' },
      { id: 'b', from: 'npub1bob', content: 'from a contact', at: '' },
    ])
    render(<CommunicationPage />)
    expect(await screen.findByText('who dis')).toBeInTheDocument()
    expect(screen.queryByText(stranger)).toBeNull()
    expect(screen.getAllByRole('button', { name: 'Add as contact' })).toHaveLength(1)
    fireEvent.click(screen.getByRole('button', { name: 'Add as contact' }))
    expect(screen.getByLabelText('npub address')).toHaveValue(stranger)
  })

  it('Reply pre-selects the sender: contact nickname, or the raw npub for strangers', async () => {
    success()
    vi.mocked(listCommHistory).mockResolvedValue([
      { id: 'a', from: stranger, content: 'who dis', at: '' },
      { id: 'b', from: 'npub1bob', content: 'from a contact', at: '' },
    ])
    render(<CommunicationPage />)
    await screen.findByText('who dis')
    const replies = screen.getAllByRole('button', { name: 'Reply' })
    fireEvent.click(replies[1]!)
    expect(screen.getByLabelText('To')).toHaveValue('bob')
    fireEvent.click(replies[0]!)
    expect(screen.getByLabelText('To')).toHaveValue(stranger)
  })

  it('sends to a raw npub that is not a contact, and rejects an unknown name', async () => {
    success()
    vi.mocked(sendCommMessage).mockResolvedValue({ published_to: 1 })
    render(<CommunicationPage />)
    await screen.findByText('npub1alice')
    expect(screen.getByText(/anyone with your npub can message you/)).toBeInTheDocument()
    fireEvent.change(screen.getByLabelText('Message'), { target: { value: 'hello' } })
    fireEvent.change(screen.getByLabelText('To'), { target: { value: 'nobody' } })
    fireEvent.click(screen.getByRole('button', { name: 'Send' }))
    expect(await screen.findByText(/Enter a contact name or an address starting with npub1/)).toBeInTheDocument()
    expect(sendCommMessage).not.toHaveBeenCalled()
    fireEvent.change(screen.getByLabelText('To'), { target: { value: stranger } })
    fireEvent.click(screen.getByRole('button', { name: 'Send' }))
    await waitFor(() => expect(sendCommMessage).toHaveBeenCalledWith(stranger, 'hello'))
  })

})
