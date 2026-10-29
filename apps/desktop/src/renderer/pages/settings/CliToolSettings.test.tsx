// @vitest-environment jsdom
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, expect, it, vi } from 'vitest'
import { CliToolSettings } from './CliToolSettings'
import { setLocale } from '../../i18n'

const status = vi.fn()
const install = vi.fn()
beforeEach(() => {
  setLocale('en')
  status.mockResolvedValue({ state: 'outdated', path: '/usr/local/bin/agent24', version: '0.3.0', bundledVersion: '0.6.0', paths: [
    { path: '/usr/local/bin/agent24', link: 'other', state: 'outdated', version: '0.3.0' },
    { path: '/home/test/.agent24/bin/agent24', link: 'current', state: 'current', version: '0.6.0' },
  ] })
  Object.defineProperty(window, 'agent24', { configurable: true, value: { cliToolStatus: status, installCliTool: install } })
})
it('shows PATH and bundled versions, installs without arguments and displays the backup', async () => {
  install.mockResolvedValue({ state: 'installed', backup: '/usr/local/bin/agent24.bak-2026-10-28' })
  render(<CliToolSettings />)
  await screen.findByText('Command line tool is outdated (0.3.0) — Update')
  expect(screen.getByText('/usr/local/bin/agent24')).toBeInTheDocument()
  expect(screen.getByText(/Bundled CLI version: 0.6.0/)).toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', { name: 'Install / Update `agent24` command' }))
  await screen.findByText('Previous command saved at /usr/local/bin/agent24.bak-2026-10-28')
  expect(install).toHaveBeenCalledWith()
})
it('explains cancellation and restores the install button', async () => {
  install.mockResolvedValue({ state: 'cancelled' })
  render(<CliToolSettings />)
  const button = await screen.findByRole('button', { name: 'Install / Update `agent24` command' })
  await waitFor(() => expect(button).toBeEnabled())
  fireEvent.click(button)
  await screen.findByText('Installation cancelled. Administrator permission or replacement was not approved.')
  await waitFor(() => expect(button).toBeEnabled())
})

it('updates CLI status and installation results in Chinese and Thai', async () => {
  install.mockResolvedValue({ state: 'cancelled' })
  render(<CliToolSettings />)
  await screen.findByText('Command line tool is outdated (0.3.0) — Update')
  fireEvent.click(screen.getByRole('button', { name: 'Install / Update `agent24` command' }))
  await screen.findByText('Installation cancelled. Administrator permission or replacement was not approved.')
  act(() => setLocale('zh'))
  expect(screen.getByText('命令行工具已过期（0.3.0）— 更新')).toBeInTheDocument()
  expect(screen.getByText('已取消安装：未批准管理员授权或替换操作。')).toBeInTheDocument()
  act(() => setLocale('th'))
  expect(screen.getByText('เครื่องมือบรรทัดคำสั่งล้าสมัย (0.3.0) — อัปเดต')).toBeInTheDocument()
  expect(screen.getByText('ยกเลิกการติดตั้งแล้ว ไม่ได้อนุมัติสิทธิ์ผู้ดูแลระบบหรือการแทนที่')).toBeInTheDocument()
})

it('stops polling and listening for focus after leaving settings', async () => {
  vi.useFakeTimers()
  status.mockClear()
  try {
    const view = render(<CliToolSettings />)
    await act(async () => {})
    expect(status).toHaveBeenCalledTimes(1)
    await act(async () => { await vi.advanceTimersByTimeAsync(30_000) })
    expect(status).toHaveBeenCalledTimes(2)
    await act(async () => { fireEvent.focus(window) })
    expect(status).toHaveBeenCalledTimes(3)
    view.unmount()
    await act(async () => { await vi.advanceTimersByTimeAsync(60_000); fireEvent.focus(window) })
    expect(status).toHaveBeenCalledTimes(3)
  } finally { vi.useRealTimers() }
})

it('shows both fixed CLI paths and offers an explicit user-only repair in all three locales', async () => {
  const repair = vi.fn().mockResolvedValue({ state: 'installed', backup: null })
  Object.defineProperty(window, 'agent24', { configurable: true, value: { cliToolStatus: status, installCliTool: install, repairUserCli: repair } })
  status.mockResolvedValue({ state: 'outdated', path: '/home/test/.agent24/bin/agent24', version: '0.4.0', bundledVersion: '0.6.0', paths: [
    { path: '/usr/local/bin/agent24', link: 'current', state: 'current', version: '0.6.0' },
    { path: '/home/test/.agent24/bin/agent24', link: 'other', state: 'outdated', version: '0.4.0' },
  ] })
  render(<CliToolSettings />)
  await screen.findByText('/usr/local/bin/agent24')
  expect(screen.getByText('/home/test/.agent24/bin/agent24')).toBeInTheDocument()
  expect(screen.getByText('Linked to this App')).toBeInTheDocument()
  expect(screen.getByText('Other link or regular file')).toBeInTheDocument()
  expect(screen.getByText(/AgentEar.*0.4.0.*pairing/)).toBeInTheDocument()
  expect(repair).not.toHaveBeenCalled()
  act(() => setLocale('zh'))
  expect(screen.getByText(/AgentEar.*0.4.0.*配对失败/)).toBeInTheDocument()
  act(() => setLocale('th'))
  expect(screen.getByText(/AgentEar.*0.4.0.*จับคู่/)).toBeInTheDocument()
  act(() => setLocale('en'))
  fireEvent.click(screen.getByRole('button', { name: 'Repair user CLI' }))
  await waitFor(() => expect(repair).toHaveBeenCalledWith())
})
