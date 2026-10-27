// A3 (pre-release desktop hardening): IPC sender validation and main-window
// navigation guards. Pure of Electron imports so it can be unit tested; the
// callers hand in the live window / URL policy.

import { pathToFileURL } from 'node:url'
import type { IpcMainInvokeEvent } from 'electron'

/** The slice of an IpcMainInvokeEvent the guard looks at. */
export interface GuardableEvent {
  senderFrame?: { url: string } | null
}

export interface IpcGuardOptions {
  /** The current main window (looked up per call: the window can be recreated). */
  getMainWindow: () => { isDestroyed(): boolean; webContents: { mainFrame: unknown } } | null
  /** True iff `url` is the application's own renderer page. */
  isTrustedUrl: (url: string) => boolean
}

export type IpcGuard = <F extends (event: IpcMainInvokeEvent, ...args: never[]) => unknown>(handler: F) => F

/**
 * Wraps an ipcMain.handle handler: the call must come from the main window's
 * main frame (not a sub-frame, not another WebContents such as the Creative
 * view) and that frame must currently be showing the app's own page.
 * Fails closed by throwing, which rejects the renderer-side invoke().
 */
export function createIpcGuard(opts: IpcGuardOptions): IpcGuard {
  return (<F extends (event: IpcMainInvokeEvent, ...args: never[]) => unknown>(handler: F): F =>
    ((event: IpcMainInvokeEvent, ...args: never[]) => {
      const frame = (event as unknown as GuardableEvent | undefined)?.senderFrame
      const win = opts.getMainWindow()
      if (
        !frame ||
        !win ||
        win.isDestroyed() ||
        frame !== win.webContents.mainFrame ||
        typeof frame.url !== 'string' ||
        !opts.isTrustedUrl(frame.url)
      ) {
        throw new Error('untrusted IPC sender')
      }
      return handler(event, ...args)
    }) as F) as IpcGuard
}

/**
 * Builds the app-origin predicate.
 * - dev: the vite dev server's origin (`VITE_DEV_SERVER_URL`, default :5173)
 * - packaged: exactly the bundled renderer `index.html` file URL (query and
 *   hash ignored — the renderer may use either).
 */
export function createAppUrlPolicy(opts: { devServerUrl?: string; rendererIndexPath: string }): (url: string) => boolean {
  const devOrigin = opts.devServerUrl ? new URL(opts.devServerUrl).origin : null
  const indexUrl = pathToFileURL(opts.rendererIndexPath).href
  return (url) => {
    let parsed: URL
    try {
      parsed = new URL(url)
    } catch {
      return false
    }
    if (devOrigin) return parsed.origin === devOrigin
    if (parsed.protocol !== 'file:') return false
    parsed.search = ''
    parsed.hash = ''
    return parsed.href === indexUrl
  }
}

/** Minimal WebContents surface used by the navigation guard. */
export interface NavigableContents {
  on(event: 'will-navigate', listener: (e: { preventDefault(): void }, url: string) => void): unknown
  setWindowOpenHandler(handler: (details: { url: string }) => { action: 'deny' }): unknown
}

const EXTERNAL_SCHEMES = new Set(['http:', 'https:'])

/**
 * Main window: never navigate away from the app, never open child windows.
 * http(s) targets are handed to the system browser; everything else is dropped.
 */
export function installNavigationGuards(
  contents: NavigableContents,
  isTrustedUrl: (url: string) => boolean,
  openExternal: (url: string) => void,
): void {
  const toExternal = (url: string): void => {
    try {
      if (EXTERNAL_SCHEMES.has(new URL(url).protocol)) openExternal(url)
    } catch { /* malformed URL: drop */ }
  }
  contents.on('will-navigate', (event, url) => {
    if (isTrustedUrl(url)) return
    event.preventDefault()
    toExternal(url)
  })
  contents.setWindowOpenHandler(({ url }) => {
    toExternal(url)
    return { action: 'deny' }
  })
}
