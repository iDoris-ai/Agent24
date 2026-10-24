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

import { registerIpcHandlers, reachesHostInterface } from './index'
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

describe('the generic backend proxy never reaches the host interface (KB-HOST-01 PR-B1a)', () => {
  const proxy = async (path: string) => {
    registerIpcHandlers()
    const handler = handlers.get(IpcChannels.BackendProxy)!
    return (await handler({}, { method: 'PUT', path })) as { status: number; data: unknown }
  }

  it.each([
    '/api/v1/host/knowledge/bindings/b1?seq=1',
    '/api/v1/host',
    '/API/v1/Host/knowledge/bindings/b1',
    '/api/v1/./host/knowledge/bindings/b1',
    '/api/v1/models/../host/knowledge/bindings/b1',
    '/api/v1//host/knowledge/bindings/b1',
    '/api/v1/%68ost/knowledge/bindings/b1',
    '/api/v1/host%2Fknowledge',
    '/api/v1\\host/knowledge/bindings/b1',
    '/api/v1/%E0%A4%A',
  ])('refuses %s without sending it', async (path) => {
    const res = await proxy(path)
    expect(res.status).toBe(403)
    expect(JSON.stringify(res.data)).toContain('host_interface_private')
  })

  it.each(['/api/v1/hosts', '/api/v1/sessions?next=/api/v1/host/x'])('still proxies %s', async (path) => {
    expect(reachesHostInterface(path)).toBe(false)
  })
})
