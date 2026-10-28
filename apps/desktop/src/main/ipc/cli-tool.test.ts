import { beforeEach, expect, it, vi } from 'vitest'
import type { IpcMainInvokeEvent } from 'electron'
import { createIpcGuard } from './guard'
import { IpcChannels } from '../../shared/ipc-types'

const handlers = new Map<string, (event: IpcMainInvokeEvent, ...args: unknown[]) => unknown>()
const install = vi.fn()
const status = vi.fn()
const confirm = vi.fn()
vi.mock('electron', () => ({
  ipcMain: { handle: (name: string, handler: (event: IpcMainInvokeEvent, ...args: unknown[]) => unknown) => handlers.set(name, handler) },
  dialog: { showMessageBox: (...args: unknown[]) => confirm(...args) },
}))
vi.mock('../cli-tool', () => ({
  cliToolStatus: (...args: unknown[]) => status(...args),
  installBundledCli: (...args: unknown[]) => install(...args),
}))
import { registerCliTool } from './cli-tool'
const frame = { url: 'file:///app/index.html' }
const event = { senderFrame: frame } as IpcMainInvokeEvent
beforeEach(() => {
  vi.clearAllMocks()
  handlers.clear()
  status.mockResolvedValue({ state: 'outdated', path: '/usr/local/bin/agent24', version: '0.3.0', bundledVersion: '0.6.0' })
})
function setup() {
  return registerCliTool({
    guard: createIpcGuard({ getMainWindow: () => ({ isDestroyed: () => false, webContents: { mainFrame: frame } }), isTrustedUrl: (url) => url === frame.url }),
    executable: '/Applications/Agent24.app/Contents/MacOS/Agent24', resources: '/Applications/Agent24.app/Contents/Resources',
    packaged: true, changed: vi.fn(), openSettings: vi.fn(),
  })
}
function invoke(channel: string, ...args: unknown[]) {
  const handler = handlers.get(channel)
  if (!handler) throw new Error('Missing handler')
  return handler(event, ...args)
}
it('rejects all renderer-supplied paths and payloads before installation', () => {
  setup()
  for (const arg of ['/tmp/evil', { target: '/tmp/evil' }, undefined, null]) {
    for (const channel of [IpcChannels.CliToolInstall, IpcChannels.CliToolStatus]) {
      expect(() => invoke(channel, arg)).toThrow('no arguments')
    }
  }
  expect(install).not.toHaveBeenCalled()
  expect(status).not.toHaveBeenCalled()
})
it('applies the sender guard to every new handler', () => {
  setup()
  for (const channel of [IpcChannels.CliToolInstall, IpcChannels.CliToolStatus, IpcChannels.AppLocale]) {
    expect(() => handlers.get(channel)?.({ senderFrame: { url: 'https://evil.test' } } as IpcMainInvokeEvent)).toThrow('untrusted IPC sender')
  }
  expect(install).not.toHaveBeenCalled()
  expect(status).not.toHaveBeenCalled()
})
it('deduplicates installs and refreshes the actual PATH status afterwards', async () => {
  setup()
  install.mockResolvedValue({ state: 'installed', backup: null })
  const first = invoke(IpcChannels.CliToolInstall)
  const second = invoke(IpcChannels.CliToolInstall)
  expect(first).toBe(second)
  expect(await first).toEqual({ state: 'installed', backup: null })
  expect(install).toHaveBeenCalledOnce()
  expect(status).toHaveBeenCalledOnce()
  expect(install.mock.calls[0][0]).toMatchObject({ executable: '/Applications/Agent24.app/Contents/MacOS/Agent24' })
})
it('uses the shared locale dictionary for the tray and native confirmation', async () => {
  const controller = setup()
  await controller.refresh()
  expect(controller.trayItem()?.label).toBe('Command line tool is outdated (0.3.0) — Update')
  invoke(IpcChannels.AppLocale, 'zh')
  expect(controller.trayItem()?.label).toBe('命令行工具已过期（0.3.0）— 更新')
  expect(() => invoke(IpcChannels.AppLocale, '../../evil')).toThrow('Invalid locale')
  install.mockImplementation(async (options: { confirm: () => Promise<boolean> }) => ({ state: await options.confirm() ? 'installed' : 'cancelled' }))
  confirm.mockResolvedValue({ response: 0 })
  expect(await invoke(IpcChannels.CliToolInstall)).toEqual({ state: 'cancelled' })
  expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ buttons: ['取消', '备份并替换'], defaultId: 0, cancelId: 0 }))
})
