// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { setLocale } from '../../i18n'
import CommunicationPage from './Communication'

const groupId = 'a'.repeat(32)
const inviteId = 'b'.repeat(32)
const me = 'npub1alice'
const bob = 'npub1bob'
const group = { group_id: groupId, name: '项目组', state: 'active', creator_npub: bob, members: [me, bob], updated_at: 1700000200 }
const message = { id: 'c'.repeat(32), event_id: '', group_id: groupId, sender: bob, content: 'CIPHERTEXT', plaintext: '群消息', created_at: 1700000200, is_encrypted: true }
const envelope = (data: unknown) => ({ ok: true, status: 200, data: { ok: true, data } })
const report = (id: string, state = 'relay_accepted') => ({ group_id: groupId, logical_id: id, queued: state === 'queued' ? 1 : 0, failed: 0, recipients: [{ recipient_npub: bob, event_id: '', queue_id: '', state, relay_acks: state === 'relay_accepted' ? 1 : 0, relay_count: 1, attempts: 1, max_retries: 3 }] })
let groups = [group]
let messages = [message]
let relayState = 'relay_accepted'
const backendProxy = vi.fn(async ({ path }: { path: string }) => {
  if (path === '/api/v1/comm/groupchat') return envelope({ groups })
  if (path.includes('/invites')) return envelope({ invites: [{ invite_id: inviteId, invitee_npub: me, group_id: groupId, state: 'pending' }] })
  if (path.endsWith('/members')) return envelope({ members: [{ npub: bob, state: 'accepted', is_creator: true }, { npub: me, state: 'pending', is_creator: false }] })
  if (path.includes(`/groupchat/${groupId}/messages/`)) return envelope(report(path.split('/').at(-1) ?? '', relayState))
  if (path.includes(`/groupchat/${groupId}/messages`)) return envelope({ messages })
  if (path === '/api/v1/comm/identity') return envelope([{ nickname: 'alice', npub: me, default: true, encrypted: true }])
  if (path === '/api/v1/comm/contact') return envelope([{ nickname: 'Bob', npub: bob, role: '' }])
  if (path === '/api/v1/comm/relay') return envelope({ relays: ['wss://relay.example'], source: 'config', configured: true })
  if (path === '/api/v1/comm/daemon') return envelope({ process: { state: 'running', generation: 1, consecutive_failures: 0, reason: null }, relay_probe: null, catch_up: { state: 'unknown', last_incomplete_at_ms: null } })
  if (path.startsWith('/api/v1/comm/history')) return envelope([{ id: 'dm', sender: bob, recipient: me, plaintext: '私聊', created_at: 1700000100 }])
  if (path === '/api/v1/components') return { ok: true, status: 200, data: { components: [] } }
  throw new Error(`Unexpected GET ${path}`)
})
const commGroupCreate = vi.fn(async (_name: string, _invitees: string[]) => envelope({ group_id: groupId }))
const commGroupAccept = vi.fn(async (_invite: string) => { groups = [{ ...group, state: 'active' }]; return envelope({ group_id: groupId, state: 'active' }) })
const commGroupSend = vi.fn(async (_group: string, _body: string, id: string): Promise<{ ok: boolean; status: number; data: unknown }> => envelope(report(id)))
const commGroupRetry = vi.fn(async (_group: string, id: string) => envelope(report(id)))

beforeEach(() => {
  setLocale('zh')
  groups = [{ ...group }]; messages = [{ ...message }]; relayState = 'relay_accepted'
  vi.clearAllMocks()
  Object.assign(window, { agent24: { backendProxy, commGroupCreate, commGroupAccept, commGroupSend, commGroupRetry } })
})
afterEach(() => { cleanup(); vi.useRealTimers() })
const selectGroup = async () => fireEvent.click(await screen.findByRole('button', { name: /项目组/ }))

describe('group chat main flows', () => {
  it('mixes groups and private chats by latest time, displays plaintext chronologically and sender names', async () => {
    messages = [{ ...message, id: 'd'.repeat(32), plaintext: '较新', created_at: 1700000300 }, message]
    render(<CommunicationPage />)
    const sidebar = screen.getByRole('complementary', { name: '会话列表' })
    await waitFor(() => expect(within(sidebar).getAllByRole('button').filter(b => b.classList.contains('comm-conversation')).map(b => b.textContent)).toEqual([expect.stringContaining('项目组'), expect.stringContaining('Bob')]))
    await selectGroup()
    await screen.findByText('群消息')
    const stream = screen.getByRole('log')
    expect(stream.textContent?.indexOf('群消息')).toBeLessThan(stream.textContent?.indexOf('较新') ?? 0)
    expect(stream.textContent).toContain('Bob')
    expect(screen.queryByText('CIPHERTEXT')).toBeNull()
    expect(screen.queryByText(/已送达|已读/)).toBeNull()
  })
  it('uses one + popover and creates with selected contact npubs through dedicated IPC', async () => {
    render(<CommunicationPage />)
    fireEvent.click(screen.getByRole('button', { name: '新建会话' }))
    expect(screen.getByRole('button', { name: '添加联系人' })).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '新建群' }))
    const form = screen.getByRole('form', { name: '新建群' })
    fireEvent.change(within(form).getByLabelText('群名'), { target: { value: '新项目' } })
    expect(within(form).getByRole('button', { name: '创建' }).hasAttribute('disabled')).toBe(true)
    fireEvent.click(await within(form).findByRole('checkbox', { name: 'Bob' }))
    fireEvent.click(within(form).getByRole('button', { name: '创建' }))
    await waitFor(() => expect(commGroupCreate).toHaveBeenCalledWith('新项目', [bob]))
    await waitFor(() => expect(screen.getByRole('heading', { name: '项目组' })).toBeTruthy())
    expect(backendProxy.mock.calls.every(([req]) => !('method' in req) || req.method === 'GET')).toBe(true)
  })
  it('opens the existing add-contact form from the popover', async () => {
    render(<CommunicationPage />)
    fireEvent.click(screen.getByRole('button', { name: '新建会话' }))
    fireEvent.click(screen.getByRole('button', { name: '添加联系人' }))
    expect(screen.getByRole('form', { name: '添加联系人' })).toBeTruthy()
  })
  it('fetches invites then accepts the local pending invite', async () => {
    groups = [{ ...group, state: 'pending' }]
    render(<CommunicationPage />)
    await selectGroup()
    expect(await screen.findByText('Bob 邀请你加入群 项目组')).toBeTruthy()
    expect(screen.getByText('待加入')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '加入' }))
    await waitFor(() => expect(commGroupAccept).toHaveBeenCalledWith(inviteId))
    expect(backendProxy.mock.calls.some(([req]) => req.path === `/api/v1/comm/groupchat/${groupId}/invites`)).toBe(true)
    expect(backendProxy.mock.invocationCallOrder[backendProxy.mock.calls.findIndex(([req]) => req.path.endsWith('/invites'))]).toBeLessThan(commGroupAccept.mock.invocationCallOrder[0] ?? 0)
    await screen.findByRole('textbox', { name: '输入消息' })
  })
  it('shows member acceptance progress for a group I created', async () => {
    groups = [{ ...group, state: 'pending', creator_npub: me }]
    render(<CommunicationPage />)
    await selectGroup()
    expect(await screen.findByText(/alice.*待接受/)).toBeTruthy()
    expect(screen.queryByRole('button', { name: '加入' })).toBeNull()
  })
  it('sends with a generated logical id and retries a partial failure with the same id', async () => {
    commGroupSend.mockImplementationOnce(async (_g, _b, id) => ({ ok: false, status: 502, data: { ok: false, error: 'partial', data: report(id, 'failed') } }))
    render(<CommunicationPage />)
    await selectGroup()
    fireEvent.change(await screen.findByRole('textbox', { name: '输入消息' }), { target: { value: '大家好' } })
    fireEvent.click(screen.getByRole('button', { name: '发送' }))
    await waitFor(() => expect(commGroupSend).toHaveBeenCalledWith(groupId, '大家好', expect.stringMatching(/^[0-9a-f]{32}$/)))
    expect(await screen.findByText('失败')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '重试' }))
    const id = commGroupSend.mock.calls.at(-1)?.[2]
    await waitFor(() => expect(commGroupRetry).toHaveBeenCalledWith(groupId, id))
    expect(await screen.findByText('已发出')).toBeTruthy()
    expect(screen.queryByText(/已送达|已读/)).toBeNull()
  })
  it('resends (not retry) a message Hyphae never stored, with the same logical id', async () => {
    commGroupSend.mockImplementationOnce(async () => ({ ok: false, status: 400, data: { ok: false, error: 'invalid', message: 'group not active' } }))
    render(<CommunicationPage />)
    await selectGroup()
    fireEvent.change(await screen.findByRole('textbox', { name: '输入消息' }), { target: { value: '没落库' } })
    fireEvent.click(screen.getByRole('button', { name: '发送' }))
    expect(await screen.findByText('失败')).toBeTruthy()
    const id = commGroupSend.mock.calls.at(-1)?.[2]
    fireEvent.click(screen.getByRole('button', { name: '重试' }))
    await waitFor(() => expect(commGroupSend).toHaveBeenCalledTimes(2))
    expect(commGroupSend.mock.calls[1]).toEqual([groupId, '没落库', id])
    expect(commGroupRetry).not.toHaveBeenCalled()
    expect(await screen.findByText('已发出')).toBeTruthy()
  })
  it('never polls status for settled or history messages', async () => {
    messages = [{ ...message, sender: me }]
    render(<CommunicationPage />)
    await selectGroup()
    expect(await screen.findByText('群消息')).toBeTruthy()
    fireEvent.change(screen.getByRole('textbox', { name: '输入消息' }), { target: { value: '一条' } })
    fireEvent.click(screen.getByRole('button', { name: '发送' }))
    expect(await screen.findByText('已发出')).toBeTruthy()
    const statusReads = backendProxy.mock.calls.filter(([req]) => req.path.includes(`/groupchat/${groupId}/messages/`))
    expect(statusReads).toHaveLength(0)
  })
  it('keeps queued messages sending until polling observes relay acceptance', async () => {
    relayState = 'queued'
    commGroupSend.mockImplementationOnce(async (_g, _b, id) => envelope(report(id, 'queued')))
    vi.useFakeTimers()
    render(<CommunicationPage />)
    await act(async () => {})
    fireEvent.click(screen.getByRole('button', { name: /项目组/ }))
    await act(async () => {})
    fireEvent.change(screen.getByRole('textbox', { name: '输入消息' }), { target: { value: '排队消息' } })
    await act(async () => { fireEvent.click(screen.getByRole('button', { name: '发送' })) })
    expect(commGroupSend).toHaveBeenCalledTimes(1)
    expect(screen.getByText('发送中')).toBeTruthy()
    expect(screen.queryByText('已发出')).toBeNull()
    relayState = 'relay_accepted'
    await act(async () => { await vi.advanceTimersByTimeAsync(10_000) })
    expect(screen.getByText('已发出')).toBeTruthy()
  })
})
