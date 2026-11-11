import { beforeEach, expect, it, vi } from 'vitest'
import type { IpcMainInvokeEvent } from 'electron'
import { createIpcGuard } from './guard'
import { IpcChannels, type ResidentServiceResult } from '../../shared/ipc-types'

const handlers = new Map<string, (event: IpcMainInvokeEvent, ...args: unknown[]) => unknown>()
vi.mock('electron', () => ({
  ipcMain: { handle: (name: string, handler: (event: IpcMainInvokeEvent, ...args: unknown[]) => unknown) => handlers.set(name, handler) },
}))
import { registerResidentService } from './resident-service'

const frame = { url: 'file:///app/index.html' }
const trusted = { senderFrame: frame } as IpcMainInvokeEvent
const stranger = { senderFrame: { url: 'https://evil.example/' } } as IpcMainInvokeEvent
const status = vi.fn()
const set = vi.fn()
beforeEach(() => {
  vi.resetAllMocks()
  handlers.clear()
  status.mockReturnValue({ available: true, installed: false })
  set.mockResolvedValue({ ok: true })
  registerResidentService({
    guard: createIpcGuard({ getMainWindow: () => ({ isDestroyed: () => false, webContents: { mainFrame: frame } }), isTrustedUrl: (url) => url === frame.url }),
    status, set,
  })
})
const invoke = (channel: string, event: IpcMainInvokeEvent, ...args: unknown[]) => {
  const handler = handlers.get(channel)
  if (!handler) throw new Error(`no handler for ${channel}`)
  return handler(event, ...args)
}

it('registers both channels', () => {
  expect([...handlers.keys()].sort()).toEqual([IpcChannels.ResidentServiceSet, IpcChannels.ResidentServiceStatus].sort())
})

it('status takes no argument and returns what main says', async () => {
  expect(await invoke(IpcChannels.ResidentServiceStatus, trusted)).toEqual({ available: true, installed: false })
  await expect(Promise.resolve().then(() => invoke(IpcChannels.ResidentServiceStatus, trusted, 'x'))).rejects.toThrow()
})

it('set takes exactly one boolean', async () => {
  expect(await invoke(IpcChannels.ResidentServiceSet, trusted, true)).toEqual({ ok: true })
  expect(set).toHaveBeenCalledWith(true)
  for (const args of [[], ['true'], [1], [true, true], [null]]) {
    set.mockClear()
    await expect(Promise.resolve().then(() => invoke(IpcChannels.ResidentServiceSet, trusted, ...args))).rejects.toThrow()
    expect(set).not.toHaveBeenCalled()
  }
})

it('a set while another runs gets that one\'s result, not a second run', async () => {
  let finish!: (r: ResidentServiceResult) => void
  set.mockImplementationOnce(() => new Promise<ResidentServiceResult>((r) => { finish = r }))
  const first = invoke(IpcChannels.ResidentServiceSet, trusted, false)
  const second = invoke(IpcChannels.ResidentServiceSet, trusted, false)
  await vi.waitFor(() => expect(finish).toBeDefined())
  finish({ ok: false, stage: 'shutdown', code: 'presence_denied' })
  expect(await first).toEqual({ ok: false, stage: 'shutdown', code: 'presence_denied' })
  expect(await second).toEqual({ ok: false, stage: 'shutdown', code: 'presence_denied' })
  expect(set).toHaveBeenCalledTimes(1)
  set.mockResolvedValueOnce({ ok: true })
  expect(await invoke(IpcChannels.ResidentServiceSet, trusted, true)).toEqual({ ok: true })
  expect(set).toHaveBeenCalledTimes(2)
})

it('a set the other way while one runs is refused as busy, not given that one\'s result (Codex B4c2 C2)', async () => {
  let finish!: (r: ResidentServiceResult) => void
  set.mockImplementationOnce(() => new Promise<ResidentServiceResult>((r) => { finish = r }))
  const on = invoke(IpcChannels.ResidentServiceSet, trusted, true)
  expect(await invoke(IpcChannels.ResidentServiceSet, trusted, false)).toEqual({ ok: false, stage: 'shutdown', code: 'busy' })
  await vi.waitFor(() => expect(finish).toBeDefined())
  finish({ ok: true })
  expect(await on).toEqual({ ok: true })
  expect(set).toHaveBeenCalledTimes(1)
  expect(set).toHaveBeenCalledWith(true)
  expect(await invoke(IpcChannels.ResidentServiceSet, trusted, false)).toEqual({ ok: true })
  expect(set).toHaveBeenLastCalledWith(false)
})

it('a set that rejects frees the slot for the next one', async () => {
  set.mockRejectedValueOnce(new Error('boom'))
  await expect(invoke(IpcChannels.ResidentServiceSet, trusted, true)).rejects.toThrow('boom')
  expect(await invoke(IpcChannels.ResidentServiceSet, trusted, false)).toEqual({ ok: true })
})

it('an untrusted frame is refused on both channels', async () => {
  await expect(Promise.resolve().then(() => invoke(IpcChannels.ResidentServiceStatus, stranger))).rejects.toThrow()
  await expect(Promise.resolve().then(() => invoke(IpcChannels.ResidentServiceSet, stranger, true))).rejects.toThrow()
  expect(status).not.toHaveBeenCalled()
  expect(set).not.toHaveBeenCalled()
})
