import { EventEmitter } from 'node:events'
import { describe, expect, it, vi } from 'vitest'
import { createAppUrlPolicy, createIpcGuard, installNavigationGuards } from './guard'

describe('A3 createAppUrlPolicy', () => {
  it('packaged: only the bundled index.html file URL (query/hash ignored)', () => {
    const ok = createAppUrlPolicy({ rendererIndexPath: '/Applications/Agent24.app/Contents/Resources/app/dist/renderer/index.html' })
    const base = 'file:///Applications/Agent24.app/Contents/Resources/app/dist/renderer/index.html'
    expect(ok(base)).toBe(true)
    expect(ok(`${base}#/chat`)).toBe(true)
    expect(ok(`${base}?x=1`)).toBe(true)
    expect(ok('file:///etc/passwd')).toBe(false)
    expect(ok('file:///Applications/Agent24.app/Contents/Resources/app/dist/renderer/other.html')).toBe(false)
    expect(ok('https://evil.example/')).toBe(false)
    expect(ok('not a url')).toBe(false)
  })
  it('dev: only the vite origin', () => {
    const ok = createAppUrlPolicy({ devServerUrl: 'http://localhost:5173', rendererIndexPath: '/x/index.html' })
    expect(ok('http://localhost:5173/')).toBe(true)
    expect(ok('http://localhost:5173/src/main.tsx')).toBe(true)
    expect(ok('http://localhost:5174/')).toBe(false)
    expect(ok('http://localhost.evil.com:5173/')).toBe(false)
    expect(ok('file:///x/index.html')).toBe(false)
  })
})

describe('A3 createIpcGuard', () => {
  const frame = { url: 'app://main' }
  const win = { isDestroyed: () => false, webContents: { mainFrame: frame } }
  const mk = (w: typeof win | null = win) => createIpcGuard({ getMainWindow: () => w, isTrustedUrl: (u) => u === 'app://main' })

  it('passes the handler result through for the main frame at the app URL', () => {
    const h = vi.fn((_e: never, a: number) => a + 1)
    expect(mk()(h as never)({ senderFrame: frame } as never, 1 as never)).toBe(2)
  })
  it.each([
    ['forged frame object', { senderFrame: { url: 'app://main' } }],
    ['null frame', { senderFrame: null }],
    ['no frame', {}],
    ['foreign url on the main frame', null],
  ])('rejects: %s', (_n, ev) => {
    const evil = { url: 'https://evil.example' }
    const guard = createIpcGuard({
      getMainWindow: () => (ev === null ? { isDestroyed: () => false, webContents: { mainFrame: evil } } : win),
      isTrustedUrl: (u) => u === 'app://main',
    })
    const h = vi.fn()
    expect(() => guard(h as never)((ev ?? { senderFrame: evil }) as never)).toThrow(/untrusted/)
    expect(h).not.toHaveBeenCalled()
  })
  it('rejects when there is no window or it is destroyed', () => {
    const h = vi.fn()
    expect(() => mk(null)(h as never)({ senderFrame: frame } as never)).toThrow(/untrusted/)
    expect(() => mk({ ...win, isDestroyed: () => true })(h as never)({ senderFrame: frame } as never)).toThrow(/untrusted/)
  })
})

describe('A3 installNavigationGuards', () => {
  function setup() {
    const wc = new EventEmitter() as EventEmitter & { setWindowOpenHandler: (h: (d: { url: string }) => { action: 'deny' }) => void }
    let open!: (d: { url: string }) => { action: 'deny' }
    wc.setWindowOpenHandler = (h) => { open = h }
    const openExternal = vi.fn()
    installNavigationGuards(wc as never, (u) => u.startsWith('app://main'), openExternal)
    const nav = (url: string): boolean => {
      const ev = { preventDefault: vi.fn() }
      wc.emit('will-navigate', ev, url)
      return ev.preventDefault.mock.calls.length > 0
    }
    return { nav, open: (url: string) => open({ url }), openExternal }
  }
  it('allows in-app navigation, blocks everything else', () => {
    const { nav, openExternal } = setup()
    expect(nav('app://main/#/chat')).toBe(false)
    expect(nav('https://evil.example/')).toBe(true)
    expect(openExternal).toHaveBeenCalledWith('https://evil.example/')
    expect(nav('file:///etc/passwd')).toBe(true)
    expect(nav('javascript:alert(1)')).toBe(true)
    expect(openExternal).toHaveBeenCalledTimes(1)
  })
  it('window.open is always denied; http(s) goes to the system browser only', () => {
    const { open, openExternal } = setup()
    expect(open('https://example.com/')).toEqual({ action: 'deny' })
    expect(openExternal).toHaveBeenCalledWith('https://example.com/')
    expect(open('file:///etc/passwd')).toEqual({ action: 'deny' })
    expect(open('app://main/')).toEqual({ action: 'deny' })
    expect(openExternal).toHaveBeenCalledTimes(1)
  })
})
