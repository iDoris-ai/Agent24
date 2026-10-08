// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  addContact,
  CommApiError,
  getCommStatus,
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

  it('issues one start, stop, and unlock request with the exact remember option', async () => {
    const proxy = setProxy(((({ path }: { path: string }) => {
      if (path.endsWith('/unlock')) return successful({ unlocked: true, remembered: false })
      return successful(status)
    }) as never))

    await expect(startCommDaemon()).resolves.toEqual(status)
    await expect(stopCommDaemon()).resolves.toEqual(status)
    await expect(unlockComm('secret-one')).resolves.toEqual({ unlocked: true, remembered: false })
    await expect(unlockComm('secret-two', true)).resolves.toEqual({ unlocked: true, remembered: false })
    expect(proxy.mock.calls.map(([request]) => request)).toEqual([
      { method: 'POST', path: '/api/v1/comm/daemon/start' },
      { method: 'POST', path: '/api/v1/comm/daemon/stop' },
      { method: 'POST', path: '/api/v1/comm/unlock', body: { password: 'secret-one', remember: false } },
      { method: 'POST', path: '/api/v1/comm/unlock', body: { password: 'secret-two', remember: true } },
    ])
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

  it('adds a contact with exactly one POST request matching the input, omitting a blank role', async () => {
    const proxy = setProxy((() => successful({})) as never)
    await expect(addContact('bob', 'npub1bob')).resolves.toBeUndefined()
    expect(proxy).toHaveBeenCalledTimes(1)
    expect(proxy.mock.calls[0][0]).toEqual({
      method: 'POST',
      path: '/api/v1/comm/contact',
      body: { nickname: 'bob', npub: 'npub1bob' },
    })
  })

  it('includes role in the contact add request body when given', async () => {
    const proxy = setProxy((() => successful({})) as never)
    await addContact('bob', 'npub1bob', 'human')
    expect(proxy.mock.calls[0][0]).toEqual({
      method: 'POST',
      path: '/api/v1/comm/contact',
      body: { nickname: 'bob', npub: 'npub1bob', role: 'human' },
    })
  })

  it('surfaces a server rejection for contact add without retrying', async () => {
    const proxy = setProxy((() => ({
      ok: false,
      status: 400,
      data: { ok: false, error: 'invalid', message: 'npub is not well-formed' },
    })) as never)
    await expect(addContact('bob', 'not-a-npub')).rejects.toMatchObject({ name: 'CommApiError', status: 400 })
    expect(proxy).toHaveBeenCalledTimes(1)
  })

  it('converts IPC/network failures into errors without retrying', async () => {
    const proxy = setProxy((() => Promise.reject(new Error('socket reset'))) as never)
    await expect(listCommRelays()).rejects.toThrow('COMM backend request failed')
    expect(proxy).toHaveBeenCalledTimes(1)
  })

  it('never reflects the password in unlock errors and does not retry', async () => {
    const password = 'super-secret-value'
    const proxy = setProxy((() => ({
      ok: false,
      status: 401,
      data: { error: { code: 'locked', message: `invalid password ${password}` } },
    })) as never)
    const log = vi.spyOn(console, 'error')

    let rejection: unknown
    try {
      await unlockComm(password, true)
    } catch (error) {
      rejection = error
    }
    expect(rejection).toBeInstanceOf(CommApiError)
    expect((rejection as Error).message).not.toContain(password)
    expect((rejection as CommApiError).code).toBe('locked')
    expect(proxy).toHaveBeenCalledTimes(1)
    expect(log).not.toHaveBeenCalled()
  })

  it('does not reflect a rejected IPC error that happens to contain the password', async () => {
    const password = 'ipc-secret'
    const proxy = setProxy((() => Promise.reject(new Error(`native failure: ${password}`))) as never)
    await expect(unlockComm(password)).rejects.toThrow('Unlock request failed')
    expect(proxy).toHaveBeenCalledTimes(1)
  })
})
