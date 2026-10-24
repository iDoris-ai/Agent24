import { describe, it, expect, vi } from 'vitest'

const handlers = vi.hoisted(() => new Map<string, (...args: unknown[]) => unknown>())

vi.mock('electron', () => ({
  app: { getPath: () => '/tmp', getVersion: () => '0.0.0', on: vi.fn() },
  BrowserWindow: { getAllWindows: () => [], fromWebContents: () => null },
  dialog: {},
  shell: {},
  ipcMain: { handle: (channel: string, fn: (...args: unknown[]) => unknown) => handlers.set(channel, fn), on: vi.fn() },
}))

vi.mock('../backend-manager', () => ({
  getBackendEndpoint: () => ({ port: 4100 }),
  daemonConnection: () => ({ port: 4100, authHeaders: { Authorization: 'Bearer secret' } }),
}))

import { registerIpcHandlers } from './index'
import { IpcChannels } from '../../shared/ipc-types'

describe('backend:endpoint hands the renderer the port only (KB-HOST-01 PR-A1)', () => {
  it('returns exactly {port}, with no credential, while main holds one', async () => {
    registerIpcHandlers()
    const handler = handlers.get(IpcChannels.BackendEndpoint)
    expect(handler).toBeDefined()
    const result = await handler!({})
    expect(result).toEqual({ port: 4100 })
    expect(JSON.stringify(result)).not.toContain('secret')
  })
})
