// @vitest-environment jsdom
// M5 B4c2: Settings → General → background service switch.
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, expect, it, vi } from 'vitest'
import { ResidentServiceSettings } from './ResidentServiceSettings'
import { setLocale } from '../../i18n'
import type { ResidentServiceResult } from '../../../shared/ipc-types'

const status = vi.fn()
const set = vi.fn()
beforeEach(() => {
  setLocale('en')
  status.mockReset()
  set.mockReset()
  status.mockResolvedValue({ available: true, installed: false })
  set.mockResolvedValue({ ok: true })
  Object.defineProperty(window, 'agent24', { configurable: true, value: { residentServiceStatus: status, setResidentService: set } })
})
const label = 'Run in the background and start at login'

it('is not shown where it cannot work (not the packaged macOS app)', async () => {
  status.mockResolvedValue({ available: false, installed: false })
  const { container } = render(<ResidentServiceSettings />)
  await waitFor(() => expect(status).toHaveBeenCalled())
  expect(container).toBeEmptyDOMElement()
})

it('is not shown when the status cannot be read', async () => {
  status.mockRejectedValue(new Error('no main'))
  const { container } = render(<ResidentServiceSettings />)
  await waitFor(() => expect(status).toHaveBeenCalled())
  expect(container).toBeEmptyDOMElement()
})

it('is checked exactly while the LaunchAgent is there', async () => {
  status.mockResolvedValue({ available: true, installed: true })
  render(<ResidentServiceSettings />)
  expect(await screen.findByRole('checkbox', { name: label })).toBeChecked()
})

it('turning it on calls set(true) once, is disabled meanwhile, then reads the status again', async () => {
  let finish!: (r: ResidentServiceResult) => void
  set.mockImplementationOnce(() => new Promise<ResidentServiceResult>((r) => { finish = r }))
  render(<ResidentServiceSettings />)
  const box = await screen.findByRole('checkbox', { name: label })
  expect(box).not.toBeChecked()
  status.mockResolvedValue({ available: true, installed: true })
  fireEvent.click(box)
  expect(set).toHaveBeenCalledWith(true)
  await waitFor(() => expect(box).toBeDisabled())
  expect(screen.getByText('Applying…')).toBeInTheDocument()
  fireEvent.click(box)
  expect(set).toHaveBeenCalledTimes(1)
  finish({ ok: true })
  await waitFor(() => expect(box).toBeEnabled())
  expect(box).toBeChecked()
  expect(status).toHaveBeenCalledTimes(2)
})

it('turning it off calls set(false)', async () => {
  status.mockResolvedValue({ available: true, installed: true })
  render(<ResidentServiceSettings />)
  fireEvent.click(await screen.findByRole('checkbox', { name: label }))
  expect(set).toHaveBeenCalledWith(false)
})

it.each([
  [{ ok: false, stage: 'start', code: 'not_started' }, 'Could not turn on the background service (not_started). Agent24 runs inside this app.'],
  [{ ok: false, stage: 'install', code: 'exit_1' }, 'Could not turn on the background service (exit_1). Agent24 runs inside this app.'],
  [{ ok: false, stage: 'shutdown', code: 'presence_denied' }, 'The background service was not stopped (presence_denied). Nothing was changed.'],
  [{ ok: false, stage: 'shutdown', code: 'not_connected' }, 'Agent24 is not connected to the background service, so it cannot ask to stop it. Choose “Start daemon” in the tray menu to connect, then turn this off.'],
  [{ ok: false, stage: 'uninstall', code: 'EACCES' }, 'The background service stopped, but its login item could not be removed (EACCES). Run `agent24 service uninstall`.'],
] as const)('a failure is shown by its stage: %o', async (result, text) => {
  set.mockResolvedValue(result)
  render(<ResidentServiceSettings />)
  fireEvent.click(await screen.findByRole('checkbox', { name: label }))
  expect(await screen.findByRole('alert')).toHaveTextContent(text)
})

it('a set that throws is shown as a failure of that direction, and the switch comes back', async () => {
  set.mockRejectedValue(new Error('ipc'))
  render(<ResidentServiceSettings />)
  const box = await screen.findByRole('checkbox', { name: label })
  fireEvent.click(box)
  expect(await screen.findByRole('alert')).toHaveTextContent('Could not turn on the background service (ipc_failed).')
  await waitFor(() => expect(box).toBeEnabled())
})

it('a new change clears the last failure', async () => {
  set.mockResolvedValueOnce({ ok: false, stage: 'start', code: 'not_started' })
  render(<ResidentServiceSettings />)
  const box = await screen.findByRole('checkbox', { name: label })
  fireEvent.click(box)
  await screen.findByRole('alert')
  await waitFor(() => expect(box).toBeEnabled())
  fireEvent.click(box)
  await waitFor(() => expect(screen.queryByRole('alert')).toBeNull())
})

it('reads the status again on focus: a change made elsewhere shows (Codex B4c2 C3)', async () => {
  render(<ResidentServiceSettings />)
  const box = await screen.findByRole('checkbox', { name: label })
  expect(box).not.toBeChecked()
  status.mockResolvedValue({ available: true, installed: true })
  fireEvent.focus(window)
  await waitFor(() => expect(box).toBeChecked())
})

it('reads the status again every 10 s, and stops when it is gone', async () => {
  vi.useFakeTimers()
  try {
    const { unmount } = render(<ResidentServiceSettings />)
    await vi.advanceTimersByTimeAsync(0)
    const before = status.mock.calls.length
    await vi.advanceTimersByTimeAsync(10_000)
    expect(status.mock.calls.length).toBe(before + 1)
    unmount()
    await vi.advanceTimersByTimeAsync(30_000)
    expect(status.mock.calls.length).toBe(before + 1)
  } finally {
    vi.useRealTimers()
  }
})

it('a change refused as busy says so', async () => {
  set.mockResolvedValue({ ok: false, stage: 'install', code: 'busy' })
  render(<ResidentServiceSettings />)
  fireEvent.click(await screen.findByRole('checkbox', { name: label }))
  expect(await screen.findByRole('alert')).toHaveTextContent('Another change is still being applied. Try again when it finishes.')
})

it('reads in Chinese and Thai', async () => {
  setLocale('zh')
  const { unmount } = render(<ResidentServiceSettings />)
  expect(await screen.findByRole('checkbox', { name: '在后台运行并随登录启动' })).toBeInTheDocument()
  expect(screen.getByText('后台服务')).toBeInTheDocument()
  unmount()
  setLocale('th')
  render(<ResidentServiceSettings />)
  expect(await screen.findByRole('checkbox', { name: 'ทำงานเบื้องหลังและเริ่มเมื่อเข้าสู่ระบบ' })).toBeInTheDocument()
})
