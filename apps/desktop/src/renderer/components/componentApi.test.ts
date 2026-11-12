import { afterEach, describe, expect, it, vi } from 'vitest'
import { ComponentApiError, listComponents, parseComponentList } from './componentApi'

const ROW = { id: 'idoris', version: '0.4.0', state: 'ready', provides: [{ id: 'model-gateway' }] }

function code(fn: () => unknown): string | undefined {
  try {
    fn()
  } catch (error) {
    return error instanceof ComponentApiError ? error.code : 'other'
  }
  return undefined
}

describe('parseComponentList', () => {
  it('accepts a full row and a redacted row', () => {
    const rows = parseComponentList({
      components: [
        {
          ...ROW,
          form: 'resident',
          reason: 'ok',
          kernel_grants: ['events'],
          declared: { kernel: ['events', 'models'], operations: [] },
        },
        { id: 'hyphae', version: null, state: 'stopped', provides: [] },
      ],
    })
    expect(rows).toEqual([
      {
        ...ROW,
        form: 'resident',
        reason: 'ok',
        kernel_grants: ['events'],
        declared: { kernel: ['events', 'models'], operations: [] },
      },
      { id: 'hyphae', version: null, state: 'stopped', provides: [] },
    ])
  })

  // One case per guard: removing that guard lets the malformed payload through.
  it.each([
    ['body is not an object', null],
    ['body is an array', []],
    ['components is not an array', { components: {} }],
    ['row is not an object', { components: ['idoris'] }],
    ['id is empty', { components: [{ ...ROW, id: '' }] }],
    ['state is unknown', { components: [{ ...ROW, state: 'running' }] }],
    ['version is a number', { components: [{ ...ROW, version: 4 }] }],
    ['provides is not an array', { components: [{ ...ROW, provides: 'model-gateway' }] }],
    ['a provide has no id', { components: [{ ...ROW, provides: [{}] }] }],
    ['form is unknown', { components: [{ ...ROW, form: 'daemon' }] }],
    ['reason is not a string', { components: [{ ...ROW, reason: 7 }] }],
    ['kernel_grants is not a string list', { components: [{ ...ROW, kernel_grants: ['events', 1] }] }],
    ['declared omits operations', { components: [{ ...ROW, declared: { kernel: [] } }] }],
  ])('rejects a payload whose %s', (_label, payload) => {
    expect(code(() => parseComponentList(payload))).toBe('invalid_response')
  })
})

describe('listComponents', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  function stubProxy(impl: () => Promise<unknown>) {
    const backendProxy = vi.fn(impl)
    vi.stubGlobal('window', { agent24: { backendProxy } })
    return backendProxy
  }

  it('reads only GET /api/v1/components', async () => {
    const proxy = stubProxy(async () => ({ ok: true, data: { components: [ROW] } }))
    await expect(listComponents()).resolves.toEqual([ROW])
    expect(proxy).toHaveBeenCalledWith({ method: 'GET', path: '/api/v1/components' })
  })

  it('maps a proxy failure to backend_not_ready and a non-ok answer to request_failed', async () => {
    stubProxy(async () => { throw new Error('no daemon') })
    await expect(listComponents()).rejects.toMatchObject({ code: 'backend_not_ready' })
    stubProxy(async () => ({ ok: false, status: 503, data: { components: [ROW] } }))
    await expect(listComponents()).rejects.toMatchObject({ code: 'request_failed' })
  })
})
