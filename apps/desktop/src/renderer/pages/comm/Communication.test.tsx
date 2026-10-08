// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import CommunicationPage from './Communication'
import {
  addContact, getCommStatus, listCommContacts, listCommIdentities, listCommRelays,
  startCommDaemon, stopCommDaemon, unlockComm,
  type CommDaemonStatus,
} from './api'

vi.mock('./api', () => ({
  addContact: vi.fn(),
  getCommStatus: vi.fn(), listCommContacts: vi.fn(), listCommIdentities: vi.fn(), listCommRelays: vi.fn(),
  startCommDaemon: vi.fn(), stopCommDaemon: vi.fn(), unlockComm: vi.fn(),
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
  vi.mocked(addContact).mockResolvedValue(undefined)
}

afterEach(() => { cleanup(); vi.clearAllMocks() })

describe('CommunicationPage', () => {
  it('renders identities, contacts, relay source, and independent status dimensions', async () => {
    success()
    render(<CommunicationPage />)
    expect(await screen.findByText('npub1alice')).toBeInTheDocument()
    expect(screen.getAllByText('默认身份')).toHaveLength(2)
    expect(screen.getByText('npub1bob')).toBeInTheDocument()
    expect(screen.getAllByText('wss://relay.example')).toHaveLength(2)
    expect(screen.getByText('来源：config · 已配置')).toBeInTheDocument()
    expect(screen.getByText('运行中')).toBeInTheDocument()
    expect(screen.getByText('最近一次握手成功')).toBeInTheDocument()
    expect(screen.getByText('状态未知')).toBeInTheDocument()
    expect(screen.queryByText(/补收完成|已送达/)).not.toBeInTheDocument()
    expect(startCommDaemon).not.toHaveBeenCalled()
    expect(stopCommDaemon).not.toHaveBeenCalled()
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
    render(<CommunicationPage />)
    const input = screen.getByLabelText('Keystore 口令') as HTMLInputElement
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

  it('submits exactly one add-contact request for valid input, then refreshes and clears the form', async () => {
    success()
    render(<CommunicationPage />)
    await screen.findByText('npub1alice')
    fireEvent.change(screen.getByLabelText('昵称'), { target: { value: 'carol' } })
    fireEvent.change(screen.getByLabelText('公钥（npub）'), { target: { value: 'npub1qpzry9x8gf2tvdw0s3jn54khce6mua7l' } })
    fireEvent.change(screen.getByLabelText('角色（可选）'), { target: { value: 'friend' } })
    const newContact = { nickname: 'carol', npub: 'npub1qpzry9x8gf2tvdw0s3jn54khce6mua7l', role: 'friend' }
    vi.mocked(listCommContacts).mockResolvedValue([contact, newContact])
    fireEvent.click(screen.getByRole('button', { name: '添加联系人' }))
    await waitFor(() => expect(addContact).toHaveBeenCalledTimes(1))
    expect(addContact).toHaveBeenCalledWith('carol', 'npub1qpzry9x8gf2tvdw0s3jn54khce6mua7l', 'friend')
    expect(await screen.findByText(newContact.npub)).toBeInTheDocument()
    expect((screen.getByLabelText('昵称') as HTMLInputElement).value).toBe('')
  })

  it('disables the submit button for an empty nickname, a malformed npub, and while a write is pending', async () => {
    success()
    render(<CommunicationPage />)
    await screen.findByText('npub1alice')
    const submit = screen.getByRole('button', { name: '添加联系人' })
    expect(submit).toBeDisabled()
    fireEvent.change(screen.getByLabelText('昵称'), { target: { value: 'carol' } })
    fireEvent.change(screen.getByLabelText('公钥（npub）'), { target: { value: 'not-a-valid-npub' } })
    expect(submit).toBeDisabled()
    fireEvent.change(screen.getByLabelText('公钥（npub）'), { target: { value: 'npub1qpzry9x8gf2tvdw0s3jn54khce6mua7l' } })
    expect(submit).toBeEnabled()
    let finish!: (value: void) => void
    vi.mocked(addContact).mockReturnValue(new Promise((resolve) => { finish = resolve }))
    fireEvent.click(submit)
    fireEvent.click(submit)
    expect(addContact).toHaveBeenCalledTimes(1)
    expect(submit).toBeDisabled()
    finish(undefined)
    await waitFor(() => expect(submit).toBeDisabled())
  })

  it('keeps the entered values and the existing list, and shows no success notice, when the server rejects the add', async () => {
    success()
    vi.mocked(addContact).mockRejectedValueOnce(Object.assign(new Error('nickname must not be empty'), { code: 'invalid' }))
    render(<CommunicationPage />)
    await screen.findByText('npub1alice')
    fireEvent.change(screen.getByLabelText('昵称'), { target: { value: 'carol' } })
    fireEvent.change(screen.getByLabelText('公钥（npub）'), { target: { value: 'npub1qpzry9x8gf2tvdw0s3jn54khce6mua7l' } })
    fireEvent.click(screen.getByRole('button', { name: '添加联系人' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('nickname must not be empty')
    expect(screen.queryByRole('status')).not.toBeInTheDocument()
    expect((screen.getByLabelText('昵称') as HTMLInputElement).value).toBe('carol')
    expect(screen.getByText('npub1bob')).toBeInTheDocument()
    expect(screen.queryByText('npub1qpzry9x8gf2tvdw0s3jn54khce6mua7l')).not.toBeInTheDocument()
  })

  it('uses only IPC proxy wrappers and never invokes fetch or storage for secrets', async () => {
    success()
    const fetchSpy = vi.spyOn(window, 'fetch')
    const storageSet = vi.spyOn(Storage.prototype, 'setItem')
    render(<CommunicationPage />)
    await screen.findByText('npub1alice')
    fireEvent.change(screen.getByLabelText('Keystore 口令'), { target: { value: 'do-not-store' } })
    fireEvent.click(screen.getByRole('button', { name: '解锁' }))
    await waitFor(() => expect(unlockComm).toHaveBeenCalled())
    expect(fetchSpy).not.toHaveBeenCalled()
    expect(storageSet).not.toHaveBeenCalled()
    expect(screen.queryByText('do-not-store')).not.toBeInTheDocument()
  })
})
