// @vitest-environment jsdom
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, expect, it, vi } from 'vitest'
import { CliToolSettings } from './CliToolSettings'
import { setLocale } from '../../i18n'

const status = vi.fn()
const install = vi.fn()
beforeEach(() => {
  setLocale('en')
  status.mockResolvedValue({ state: 'outdated', path: '/usr/local/bin/agent24', version: '0.3.0', bundledVersion: '0.6.0' })
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
