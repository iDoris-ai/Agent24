import { EventEmitter } from 'node:events'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { ChildProcess } from 'node:child_process'
import { BackendManager, daemonChildEnv, type ExternalDaemonSource } from './backend-manager'

const spawnMock = vi.hoisted(() => vi.fn())

vi.mock('node:child_process', () => ({
  spawn: spawnMock,
  fork: spawnMock,
}))

function fakeChild(): ChildProcess {
  const child = new EventEmitter() as ChildProcess
  Object.assign(child, {
    pid: 1,
    stdout: null,
    stderr: null,
    stdin: null,
    kill: vi.fn().mockReturnValue(true),
  })
  return child
}

function absent(): ExternalDaemonSource {
  return {
    read: () => null,
    checkHealth: vi.fn().mockResolvedValue(false),
    fetchHealth: vi.fn().mockResolvedValue({ ok: false, protocolVersion: null }),
    serviceManaged: () => false,
    acceptsToken: vi.fn().mockResolvedValue(false),
    requestShutdown: vi.fn().mockResolvedValue(null),
    isAlive: () => false,
  }
}

describe('daemonChildEnv', () => {
  it('drops the development inbound variables and keeps the rest', () => {
    const env = daemonChildEnv({
      PATH: '/usr/bin',
      HOME: '/Users/jason',
      A24_DEV_TOOL: '1',
      A24_INBOUND_DEV_ALLOWLIST: 'wechat:alice',
    })
    expect(env).not.toHaveProperty('A24_DEV_TOOL')
    expect(env).not.toHaveProperty('A24_INBOUND_DEV_ALLOWLIST')
    expect(env['PATH']).toBe('/usr/bin')
    expect(env['HOME']).toBe('/Users/jason')
  })

  it('drops lowercase and mixed-case spellings', () => {
    const env = daemonChildEnv({
      PATH: '/usr/bin',
      a24_dev_tool: '1',
      A24_Dev_Tool: '1',
      a24_inbound_dev_allowlist: 'wechat:alice',
      A24_Inbound_Dev_Allowlist: 'wechat:bob',
    })
    expect(env).not.toHaveProperty('a24_dev_tool')
    expect(env).not.toHaveProperty('A24_Dev_Tool')
    expect(env).not.toHaveProperty('a24_inbound_dev_allowlist')
    expect(env).not.toHaveProperty('A24_Inbound_Dev_Allowlist')
    expect(env['PATH']).toBe('/usr/bin')
  })
})

describe('desktop daemon spawn env', () => {
  const saved = {
    tool: process.env['A24_DEV_TOOL'],
    list: process.env['A24_INBOUND_DEV_ALLOWLIST'],
    bin: process.env['AGENT24D_BIN'],
  }

  beforeEach(() => {
    spawnMock.mockReset()
    spawnMock.mockImplementation(() => fakeChild())
    process.env['A24_DEV_TOOL'] = '1'
    process.env['A24_INBOUND_DEV_ALLOWLIST'] = 'wechat:alice'
    process.env['AGENT24D_BIN'] = '/tmp/agent24d'
  })

  afterEach(() => {
    restore('A24_DEV_TOOL', saved.tool)
    restore('A24_INBOUND_DEV_ALLOWLIST', saved.list)
    restore('AGENT24D_BIN', saved.bin)
  })

  it('legacy spawn (the Windows path) does not inherit the development inbound env', async () => {
    const manager = new BackendManager({ isPackaged: true, authMode: 'legacy', external: absent() })
    try {
      await manager.start()
      expect(spawnMock).toHaveBeenCalledTimes(1)
      const [bin, args, options] = spawnMock.mock.calls[0] as [string, string[], { env: NodeJS.ProcessEnv }]
      expect(bin).toBe('/tmp/agent24d')
      expect(args).toEqual(['serve', '--port', '0'])
      expect(options.env['A24_DEV_TOOL']).toBeUndefined()
      expect(options.env['A24_INBOUND_DEV_ALLOWLIST']).toBeUndefined()
      expect(options.env['AGENT24D_BIN']).toBe('/tmp/agent24d')
    } finally {
      manager.stop()
    }
  })

  it('capabilities spawn drops the same variables', async () => {
    const manager = new BackendManager({
      isPackaged: true,
      authMode: 'capabilities',
      external: absent(),
    })
    try {
      await manager.start()
      const [, args, options] = spawnMock.mock.calls[0] as [string, string[], { env: NodeJS.ProcessEnv }]
      expect(args).toEqual(['serve', '--port', '0', '--auth-mode', 'capabilities', '--host-bootstrap-stdio'])
      expect(options.env['A24_DEV_TOOL']).toBeUndefined()
      expect(options.env['A24_INBOUND_DEV_ALLOWLIST']).toBeUndefined()
    } finally {
      manager.stop()
    }
  })
})

function restore(key: string, value: string | undefined): void {
  if (value === undefined) delete process.env[key]
  else process.env[key] = value
}
