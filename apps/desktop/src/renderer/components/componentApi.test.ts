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
          denied: [{ item: 'models', code: 'no_channel' }],
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
        denied: [{ item: 'models', code: 'no_channel' }],
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
  ])('rejects a payload whose %s', (_label, payload) => {
    expect(code(() => parseComponentList(payload))).toBe('invalid_response')
  })

  it('keeps a legal isolation value and drops a missing or illegal one', () => {
    const rows = parseComponentList({
      components: [
        { ...ROW, isolation: { state: 'isolated', mechanism: 'seatbelt' } },
        { ...ROW, id: 'documents', isolation: { state: 'not_isolated', reason: 'platform_unsupported' } },
        { ...ROW, id: 'hyphae', isolation: { state: 'not_isolated', reason: 'form_not_covered' } },
        { ...ROW, id: 'knowledge' },
        { ...ROW, id: 'open-design', isolation: null },
        { ...ROW, id: 'mcp-files', isolation: { state: 'isolated' } },
        { ...ROW, id: 'sin90', isolation: { state: 'not_isolated', reason: 'maybe' } },
        { ...ROW, id: 'cos72', isolation: 'seatbelt' },
        { ...ROW, id: 'widget', kernel_consent: 'missing' },
        { ...ROW, id: 'documents', kernel_consent: 'exempt' },
        { ...ROW, id: 'sin90', kernel_consent: 'granted' },
        { ...ROW, id: 'later', kernel_consent: 'later' },
      ],
    })
    expect(rows.map((row) => row.kernel_consent)).toEqual([
      undefined, undefined, undefined, undefined, undefined, undefined, undefined, undefined,
      'missing', 'exempt', 'granted', undefined,
    ])
    expect(rows.map((row) => row.isolation)).toEqual([
      { state: 'isolated', mechanism: 'seatbelt' },
      { state: 'not_isolated', reason: 'platform_unsupported' },
      { state: 'not_isolated', reason: 'form_not_covered' },
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
    ])
  })

  it('drops a malformed kernel field and keeps the rest of the row', () => {
    const rows = parseComponentList({
      components: [{
        ...ROW,
        form: 'resident',
        kernel_grants: ['events', 1],
        declared: { kernel: [] },
        denied: [{ item: 1, code: 'no_channel' }],
      }, {
        ...ROW,
        id: 'hyphae',
        kernel_grants: null,
        declared: null,
        denied: { item: 'events', code: 'no_channel' },
      }],
    })
    expect(rows).toEqual([
      { ...ROW, form: 'resident' },
      { ...ROW, id: 'hyphae' },
    ])
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
