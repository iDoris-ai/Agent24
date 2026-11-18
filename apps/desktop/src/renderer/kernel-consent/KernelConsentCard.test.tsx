// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BackendProxyRequest, BackendProxyResponse } from '../../shared/ipc-types'
import { setLocale } from '../i18n'
import { KernelConsentCard } from './KernelConsentCard'

const VIEW = {
  module: 'widget',
  class: 'missing',
  fingerprint: 'k=events,models;o=',
  version: '1.2.0',
  team: null,
  origin: 'local',
  isolated: true,
  capabilities: ['events', 'models'],
  consented: null,
}

let posts: BackendProxyRequest[] = []
let grant: BackendProxyResponse = { ok: true, status: 200, data: { ...VIEW, class: 'granted', consented: ['events', 'models'] } }

beforeEach(() => {
  setLocale('en')
  posts = []
  grant = { ok: true, status: 200, data: { ...VIEW, class: 'granted', consented: ['events', 'models'] } }
  Object.defineProperty(window, 'agent24', {
    configurable: true,
    value: {
      backendProxy: async (req: BackendProxyRequest) => {
        if (req.method === 'GET') return { ok: true, status: 200, data: VIEW }
        posts.push(req)
        return grant
      },
    },
  })
})

afterEach(() => cleanup())

describe('KernelConsentCard', () => {
  it('checks every capability by default and proposes only the ones left on', async () => {
    render(<KernelConsentCard name="widget" />)
    expect(await screen.findByRole('checkbox', { name: 'Send events under this module\'s name' })).toBeChecked()
    expect(screen.getByRole('checkbox', { name: 'Have the kernel call models for it' })).toBeChecked()
    expect(screen.getByText('This system cannot see a signing team')).toBeInTheDocument()
    expect(screen.getByText('Installed on this computer')).toBeInTheDocument()
    expect(screen.getByText('Isolated')).toBeInTheDocument()

    fireEvent.click(screen.getByRole('checkbox', { name: 'Have the kernel call models for it' }))
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }))
    await waitFor(() => expect(posts).toHaveLength(1))
    expect(posts[0]).toEqual({
      method: 'POST',
      path: '/api/v1/os/widget/kernel-consent',
      body: { fingerprint: 'k=events,models;o=', capabilities: ['events'] },
    })
  })

  it('sends nothing when every box is cleared, and says that is a refusal', async () => {
    render(<KernelConsentCard name="widget" />)
    await screen.findByRole('checkbox', { name: 'Send events under this module\'s name' })
    fireEvent.click(screen.getByRole('checkbox', { name: 'Send events under this module\'s name' }))
    fireEvent.click(screen.getByRole('checkbox', { name: 'Have the kernel call models for it' }))
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }))
    expect(posts).toHaveLength(0)
    expect(screen.getByRole('status')).toHaveTextContent('Nothing is checked, so this is a refusal')
  })

  it('a denied or unavailable dialog leaves the module in place and writes nothing locally', async () => {
    grant = { ok: false, status: 403, data: { error: { code: 'presence_denied', message: 'no' } } }
    const { unmount } = render(<KernelConsentCard name="widget" />)
    await screen.findByRole('button', { name: 'Continue' })
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }))
    expect(await screen.findByRole('status')).toHaveTextContent('The module stays installed, without kernel capabilities.')
    expect(posts).toHaveLength(1)
    unmount()

    grant = { ok: false, status: 403, data: { error: { code: 'presence_unavailable', message: 'no' } } }
    render(<KernelConsentCard name="widget" />)
    await screen.findByRole('button', { name: 'Continue' })
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }))
    expect(await screen.findByRole('status')).toHaveTextContent('No one is at the computer')
  })

  it('uses the host sentences in Chinese and Thai', async () => {
    render(<KernelConsentCard name="widget" />)
    await screen.findByRole('checkbox', { name: 'Send events under this module\'s name' })
    act(() => setLocale('zh'))
    expect(screen.getByRole('checkbox', { name: '以这个模块的名字发出事件' })).toBeChecked()
    expect(screen.getByRole('checkbox', { name: '让内核替它调用模型' })).toBeChecked()
    expect(screen.getByText('这个系统上看不到签名团队')).toBeInTheDocument()
    expect(screen.getByText('本机安装')).toBeInTheDocument()
    expect(screen.getByText('已隔离')).toBeInTheDocument()
    act(() => setLocale('th'))
    expect(screen.getByRole('checkbox', { name: 'ส่งเหตุการณ์ในชื่อของโมดูลนี้' })).toBeInTheDocument()
    expect(screen.getByText('ระบบนี้มองไม่เห็นทีมที่ลงนาม')).toBeInTheDocument()
    expect(screen.getByText('ติดตั้งบนเครื่องนี้')).toBeInTheDocument()
    expect(screen.getByText('แยกส่วนแล้ว')).toBeInTheDocument()
    act(() => setLocale('en'))
  })

  it('does not render a card for an exempt module', async () => {
    Object.defineProperty(window, 'agent24', {
      configurable: true,
      value: {
        backendProxy: async () => ({ ok: true, status: 200, data: { ...VIEW, class: 'exempt', capabilities: ['events'] } }),
      },
    })
    render(<KernelConsentCard name="documents" />)
    await waitFor(() => expect(window.agent24.backendProxy).toBeDefined())
    await new Promise((resolve) => setTimeout(resolve, 20))
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument()
  })
})
