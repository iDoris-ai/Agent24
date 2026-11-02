// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  CommApiError,
  addCommContact,
  COMM_IPC_TIMEOUT_MS,
  exportCommIdentity,
  listCommHistory,
  revealCommExport,
  pickCommBackup,
  restoreCommIdentity,
  sendCommMessage,
  getCommStatus,
  importCommHome,
  setCommRelay,
  installComm,
  listCommContacts,
  listCommIdentities,
  listCommRelays,
  startCommDaemon,
  stopCommDaemon,
  unlockComm,
} from './api'

const status = {
  process: {
    state: 'locked',
    generation: 3,
    consecutive_failures: 2,
    reason: 'password_rejected',
  },
  relay_probe: {
    url: 'wss://relay.example',
    connected: false,
    at_ms: 1234,
    error: 'connection refused',
  },
  catch_up: { state: 'incomplete', last_incomplete_at_ms: 5678 },
}

function successful(data: unknown) {
  return { ok: true, status: 200, data: { ok: true, data } }
}

function setProxy(implementation: (...args: never[]) => unknown) {
  const proxy = vi.fn(implementation)
  window.agent24 = { backendProxy: proxy } as never
  return proxy
}

afterEach(() => {
  vi.restoreAllMocks()
})

describe('COMM typed backend API', () => {
  it('installs through the dedicated elevated IPC instead of the generic proxy', async () => {
    const proxy = setProxy(() => { throw new Error('generic proxy must not install executables') })
    const result = { installed_version: 'v0.28.1', path: '/state/components/hyphae/v0.28.1/hyphae', restart_required: false, reason: null }
    window.agent24 = { ...window.agent24, commInstall: vi.fn(async () => successful(result)) }
    expect(await installComm()).toEqual(result)
    expect(window.agent24.commInstall).toHaveBeenCalledOnce()
    expect(proxy).not.toHaveBeenCalled()
  })

  it('unwraps nested success envelopes and calls the frozen read/status routes', async () => {
    const proxy = setProxy((({ path }: { path: string }) => {
      if (path.endsWith('/identity')) {
        return successful([{ nickname: 'alice', npub: 'npub1alice', default: true, encrypted: true }])
      }
      if (path.endsWith('/contact')) return successful([{ nickname: 'bob', npub: 'npub1bob', role: 'human' }])
      if (path.endsWith('/relay')) return successful({ relays: ['wss://relay.example'], source: 'config', configured: true })
      return successful(status)
    }) as never)

    expect(await listCommIdentities()).toEqual([
      { nickname: 'alice', npub: 'npub1alice', default: true, encrypted: true },
    ])
    expect(await listCommContacts()).toEqual([{ nickname: 'bob', npub: 'npub1bob', role: 'human' }])
    expect(await listCommRelays()).toEqual({ relays: ['wss://relay.example'], source: 'config', configured: true })
    expect(await getCommStatus()).toEqual(status)
    expect(proxy.mock.calls.map(([request]) => request)).toEqual([
      { method: 'GET', path: '/api/v1/comm/identity' },
      { method: 'GET', path: '/api/v1/comm/contact' },
      { method: 'GET', path: '/api/v1/comm/relay' },
      { method: 'GET', path: '/api/v1/comm/daemon' },
    ])
  })

  it('issues one start and stop request through the proxy, and unlock through its own IPC with only the remember flag', async () => {
    const proxy = setProxy(((() => successful(status)) as never))
    const commUnlock = vi.fn(async () => successful({ unlocked: true, remembered: false }))
    window.agent24 = { ...window.agent24, commUnlock } as never

    await expect(startCommDaemon()).resolves.toEqual(status)
    await expect(stopCommDaemon()).resolves.toEqual(status)
    await expect(unlockComm()).resolves.toEqual({ unlocked: true, remembered: false })
    await expect(unlockComm(true)).resolves.toEqual({ unlocked: true, remembered: false })
    expect(proxy.mock.calls.map(([request]) => request)).toEqual([
      { method: 'POST', path: '/api/v1/comm/daemon/start' },
      { method: 'POST', path: '/api/v1/comm/daemon/stop' },
    ])
    expect(commUnlock.mock.calls).toEqual([[false], [true]])
  })

  it('preserves HTTP error code/message including unauthorized responses', async () => {
    setProxy((() => ({
      ok: false,
      status: 401,
      data: { error: { code: 'unauthorized', message: 'token expired' } },
    })) as never)

    await expect(listCommIdentities()).rejects.toMatchObject({
      name: 'CommApiError',
      code: 'unauthorized',
      status: 401,
      message: 'token expired',
    })
  })

  it('rejects an ok:false API envelope even when the IPC/HTTP layer says ok', async () => {
    setProxy((() => ({
      ok: true,
      status: 200,
      data: { ok: false, error: { code: 'locked', message: 'password required' } },
    })) as never)

    await expect(getCommStatus()).rejects.toMatchObject({ code: 'locked', message: 'password required' })
  })

  it('fails closed for malformed envelopes and malformed critical fields', async () => {
    setProxy((() => ({ ok: true, status: 200, data: { ok: true, data: {} } })) as never)
    await expect(listCommIdentities()).rejects.toThrow('Malformed COMM response data')

    setProxy((() => ({ ok: true, status: 200, data: { ok: true, data: [] } })) as never)
    await expect(getCommStatus()).rejects.toThrow('Malformed COMM response data')

    setProxy((() => ({ ok: true, status: 200, data: { data: [] } })) as never)
    await expect(listCommContacts()).rejects.toThrow('Invalid COMM response envelope')

    setProxy((() => ({ ok: true, status: 200, data: { ok: true, data: [{ nickname: 'broken' }] } })) as never)
    await expect(listCommContacts()).rejects.toThrow('Malformed COMM response data')
  })

  it('converts IPC/network failures into errors without retrying', async () => {
    const proxy = setProxy((() => Promise.reject(new Error('socket reset'))) as never)
    await expect(listCommRelays()).rejects.toThrow('COMM backend request failed')
    expect(proxy).toHaveBeenCalledTimes(1)
  })

  it('unlock never goes through the generic proxy and a dismissed window is a cancelled error', async () => {
    const proxy = setProxy(() => { throw new Error('generic proxy must not unlock') })
    const commUnlock = vi.fn()
      .mockResolvedValueOnce({ ok: false, status: 499, data: { error: { code: 'cancelled', message: 'c' } } })
      .mockRejectedValueOnce(new Error('native failure'))
    window.agent24 = { ...window.agent24, commUnlock } as never
    await expect(unlockComm()).rejects.toMatchObject({ code: 'cancelled' })
    await expect(unlockComm()).rejects.toThrow('COMM backend request failed')
    expect(proxy).not.toHaveBeenCalled()
    expect(commUnlock).toHaveBeenCalledTimes(2)
  })

  it('real daemon error shape {ok:false,error:code,message} keeps both the code and the message', async () => {
    setProxy(() => ({
      ok: false,
      status: 409,
      data: { ok: false, error: 'not_configured', message: 'no Hyphae binary is configured; set A24_HYPHAE_BIN' },
    }))
    const failure = await listCommIdentities().catch((e: unknown) => e)
    expect(failure).toBeInstanceOf(CommApiError)
    expect(failure).toMatchObject({ code: 'not_configured', message: 'no Hyphae binary is configured; set A24_HYPHAE_BIN', status: 409 })
  })

  it('whole-home import and the relay list use dedicated IPC with no path or password from the page', async () => {
    const proxy = setProxy(() => { throw new Error('generic proxy must not import or set relays') })
    const report = { identities: 1, contacts: 2, outbox: 0, db_files: ['a.db'] }
    const commHomeImport = vi.fn()
      .mockResolvedValueOnce(successful(report))
      .mockResolvedValueOnce({ ok: false, status: 499, data: { error: { code: 'cancelled', message: 'c' } } })
    const config = { relays: ['wss://r.example'], source: 'config', configured: true }
    const commRelaySet = vi.fn(async () => successful(config))
    window.agent24 = { ...window.agent24, commHomeImport, commRelaySet } as never
    await expect(importCommHome()).resolves.toEqual(report)
    await expect(importCommHome()).rejects.toMatchObject({ code: 'cancelled' })
    expect(commHomeImport.mock.calls.every((c) => c.length === 0)).toBe(true)
    await expect(setCommRelay(['wss://r.example'])).resolves.toEqual(config)
    expect(commRelaySet).toHaveBeenCalledWith(['wss://r.example'])
    expect(proxy).not.toHaveBeenCalled()
  })
})


describe('COMM contact, send, history and export wrappers', () => {
  it('sends contact add and send through dedicated IPC, never the generic proxy', async () => {
    const proxy = setProxy(() => { throw new Error('generic proxy must not carry Elevated comm writes') })
    const commContactAdd = vi.fn(async () => successful({}))
    const commSend = vi.fn(async () => successful({ published_to: 3 }))
    window.agent24 = { ...window.agent24, commContactAdd, commSend } as never
    await addCommContact('bob', 'npub1bob', 'friend')
    await expect(sendCommMessage('bob', 'hi')).resolves.toEqual({ published_to: 3 })
    expect(commContactAdd).toHaveBeenCalledWith('bob', 'npub1bob', 'friend')
    expect(commSend).toHaveBeenCalledWith('bob', 'hi')
    expect(proxy).not.toHaveBeenCalled()
  })
  it('keeps only displayable history rows', async () => {
    setProxy(() => successful([{ id: 'e1', from: 'npub1bob', content: 'hello', created_at: 1_700_000_000 }, { id: 'x' }]))
    const rows = await listCommHistory(5)
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ id: 'e1', from: 'npub1bob', content: 'hello' })
  })
  it('exports through the dedicated IPC with the nickname ONLY (no password crosses from the page), and rejects a cancel with its code', async () => {
    const proxy = setProxy(() => { throw new Error('generic proxy must not export keys') })
    const commIdentityExport = vi.fn()
      .mockResolvedValueOnce(successful({ nickname: 'alice', npub: 'npub1a', path: '/p.ncryptsec' }))
      .mockResolvedValueOnce({ ok: false, status: 499, data: { error: { code: 'cancelled', message: 'cancelled' } } })
    window.agent24 = { ...window.agent24, commIdentityExport } as never
    await expect(exportCommIdentity('alice')).resolves.toEqual({ nickname: 'alice', npub: 'npub1a', path: '/p.ncryptsec', warnings: [] })
    await expect(exportCommIdentity('alice')).rejects.toMatchObject({ code: 'cancelled' })
    expect(commIdentityExport).toHaveBeenCalledTimes(2)
    expect(commIdentityExport.mock.calls.every((c) => c.length === 1 && c[0] === 'alice')).toBe(true)
    expect(proxy).not.toHaveBeenCalled()
  })

  it('rejects an export IPC that never returns, so the page can clear pending', async () => {
    vi.useFakeTimers()
    try {
      const commIdentityExport = vi.fn(() => new Promise(() => {}))
      window.agent24 = { ...window.agent24, commIdentityExport } as never
      const pending = exportCommIdentity('alice').then(() => 'ok' as const, (error: unknown) => error)
      await vi.advanceTimersByTimeAsync(COMM_IPC_TIMEOUT_MS)
      await expect(pending).resolves.toMatchObject({ code: 'timeout' })
    } finally {
      vi.useRealTimers()
    }
  })

  it('asks main to reveal exactly the path the page was given', async () => {
    const commRevealExport = vi.fn(async () => successful({ path: '/Users/x/Downloads/alice.ncryptsec' }))
    window.agent24 = { ...window.agent24, commRevealExport } as never
    await revealCommExport('/Users/x/Downloads/alice.ncryptsec')
    expect(commRevealExport).toHaveBeenCalledWith('/Users/x/Downloads/alice.ncryptsec')
  })
})

describe('COMM export warnings', () => {
  it('passes the daemon warnings through and ignores unknown ones', async () => {
    const commIdentityExport = vi.fn().mockResolvedValue(successful({
      nickname: 'a', npub: 'n', path: '/p',
      warnings: [{ code: 'cleanup_failed', path: '/tmp/x' }, { code: 'durability_unconfirmed' }, { code: 'something_new' }, 'junk'],
    }))
    window.agent24 = { ...window.agent24, commIdentityExport } as never
    await expect(exportCommIdentity('a')).resolves.toMatchObject({
      warnings: [{ code: 'cleanup_failed', path: '/tmp/x' }, { code: 'durability_unconfirmed' }],
    })
  })
})

describe('COMM restore wrappers', () => {
  it('pick returns only a ticket and a file name; a cancel is null', async () => {
    const commImportPick = vi.fn()
      .mockResolvedValueOnce({ ok: true, ticket: 't1', fileName: 'a.ncryptsec' })
      .mockResolvedValueOnce({ ok: false, cancelled: true })
    window.agent24 = { ...window.agent24, commImportPick } as never
    await expect(pickCommBackup()).resolves.toEqual({ ticket: 't1', fileName: 'a.ncryptsec' })
    await expect(pickCommBackup()).resolves.toBeNull()
  })
  it('pick reports a file main could not use as a backup', async () => {
    const commImportPick = vi.fn().mockResolvedValueOnce({ ok: false, cancelled: false, message: 'not a backup' })
    window.agent24 = { ...window.agent24, commImportPick } as never
    await expect(pickCommBackup()).rejects.toMatchObject({ code: 'invalid_key', message: 'not a backup' })
  })
  it('restore goes through the dedicated IPC with ticket, nickname and default ONLY (no password)', async () => {
    const proxy = setProxy(() => { throw new Error('generic proxy must not restore keys') })
    const commIdentityImport = vi.fn()
      .mockResolvedValueOnce(successful({ status: 'imported', nickname: 'alice', npub: 'npub1a', default: true }))
      .mockResolvedValueOnce(successful({ status: 'already_present', nickname: 'old', npub: 'npub1a' }))
      .mockResolvedValueOnce({ ok: false, status: 409, data: { ok: false, error: 'nickname_exists', message: 'x' } })
      .mockResolvedValueOnce({ ok: false, status: 499, data: { error: { code: 'cancelled', message: 'c' } } })
    window.agent24 = { ...window.agent24, commIdentityImport } as never
    await expect(restoreCommIdentity('t1', 'alice', true)).resolves.toEqual({ status: 'imported', nickname: 'alice', npub: 'npub1a' })
    await expect(restoreCommIdentity('t1', 'alice', false)).resolves.toMatchObject({ status: 'already_present', nickname: 'old' })
    await expect(restoreCommIdentity('t1', 'alice', false)).rejects.toMatchObject({ code: 'nickname_exists' })
    await expect(restoreCommIdentity('t1', 'alice', false)).rejects.toMatchObject({ code: 'cancelled' })
    expect(commIdentityImport.mock.calls.every((c) => c.length === 3)).toBe(true)
    expect(proxy).not.toHaveBeenCalled()
  })
})
