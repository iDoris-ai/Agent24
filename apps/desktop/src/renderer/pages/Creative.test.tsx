// @vitest-environment jsdom
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import CreativePage from './Creative'
import type { OpenDesignComponentStatusResult } from '../../shared/ipc-types'

const creativeShow = vi.fn()
const creativeRestart = vi.fn()
const creativeBounds = vi.fn()
const creativeHide = vi.fn()
const openDesignComponentStatus = vi.fn()
const openDesignComponentInstall = vi.fn()
let resizeCallback: ResizeObserverCallback | null = null
let progressListener: ((status: OpenDesignComponentStatusResult) => void) | null = null

beforeEach(() => {
  vi.clearAllMocks()
  resizeCallback = null
  progressListener = null
  creativeShow.mockResolvedValue({ ok: true, origin: 'http://127.0.0.1:7456' })
  creativeRestart.mockResolvedValue({ ok: true, origin: 'http://127.0.0.1:7456' })
  creativeBounds.mockResolvedValue(undefined)
  creativeHide.mockResolvedValue(undefined)
  // Default: the on-demand Open Design component is already installed, so
  // these pre-existing tests exercise CreativePage exactly as before this
  // component was introduced — straight through to the WebContentsView.
  openDesignComponentStatus.mockResolvedValue({ state: 'installed', dir: '/tmp/open-design' })
  Object.defineProperty(window, 'agent24', {
    value: {
      creativeShow, creativeRestart, creativeBounds, creativeHide,
      openDesignComponentStatus, openDesignComponentInstall,
      onOpenDesignComponentProgress: (cb: (status: OpenDesignComponentStatusResult) => void) => {
        progressListener = cb
        return () => { progressListener = null }
      },
    },
    configurable: true,
  })
  vi.stubGlobal('ResizeObserver', class {
    constructor(callback: ResizeObserverCallback) { resizeCallback = callback }
    observe(): void {}
    disconnect(): void {}
    unobserve(): void {}
  })
})

describe('CreativePage', () => {
  it('shows the native view, syncs resized bounds, and hides it on unmount', async () => {
    const { container, unmount } = render(<CreativePage />)
    const host = container.querySelector('.creative-host') as HTMLDivElement
    vi.spyOn(host, 'getBoundingClientRect').mockReturnValue({
      x: 220, y: 50, width: 900, height: 650,
      top: 50, right: 1120, bottom: 700, left: 220, toJSON: () => ({}),
    })

    await waitFor(() => expect(creativeShow).toHaveBeenCalled())
    await act(async () => { resizeCallback?.([], {} as ResizeObserver) })
    expect(creativeBounds).toHaveBeenLastCalledWith({ x: 220, y: 50, width: 900, height: 650 })

    unmount()
    expect(creativeHide).toHaveBeenCalledTimes(1)
  })

  it('retries a failed Creative launch and clears the error after restart', async () => {
    creativeShow.mockResolvedValueOnce({ ok: false, error: 'daemon unavailable' })
    const { container, getByRole, queryByText } = render(<CreativePage />)
    const host = container.querySelector('.creative-host') as HTMLDivElement
    vi.spyOn(host, 'getBoundingClientRect').mockReturnValue({
      x: 220, y: 50, width: 900, height: 650,
      top: 50, right: 1120, bottom: 700, left: 220, toJSON: () => ({}),
    })

    await waitFor(() => expect(queryByText(/daemon unavailable/)).not.toBeNull())
    fireEvent.click(getByRole('button', { name: '重试' }))

    await waitFor(() => expect(creativeRestart).toHaveBeenCalledWith({ x: 220, y: 50, width: 900, height: 650 }))
    await waitFor(() => expect(queryByText(/daemon unavailable/)).toBeNull())
  })

  it('shows retry UI when the initial Creative IPC rejects', async () => {
    creativeShow.mockRejectedValueOnce(new Error('launch IPC failed'))
    const { queryByText, getByRole } = render(<CreativePage />)

    await waitFor(() => expect(queryByText(/launch IPC failed/)).not.toBeNull())
    expect(getByRole('button', { name: '重试' })).toBeTruthy()
  })
})

describe('CreativePage — on-demand iDoris Design component', () => {
  it('shows the download card with the manifest size when not installed, without starting Creative', async () => {
    openDesignComponentStatus.mockResolvedValue({ state: 'not-installed', size: 50 * 1024 * 1024 })
    render(<CreativePage />)

    expect(await screen.findByText(/Design 需要额外下载 iDoris Design 组件（约 50 MB）/)).toBeTruthy()
    expect(screen.getByRole('button', { name: '下载' })).toBeTruthy()
    expect(creativeShow).not.toHaveBeenCalled()
  })

  // PR #670 review (H1): `unavailable` (no baked manifest, e.g. a dev
  // build) must still attempt `creativeShow` — CreativeServeWeb, not this
  // page, knows whether a dev checkout (A24_OPEN_DESIGN_DIR / sibling
  // checkout) makes the component unnecessary. Only `creativeShow`'s own
  // `needsDownload` result puts up the download card.
  it('H1 (dev mode): unavailable + creativeShow succeeds -> no card, no error, Creative starts', async () => {
    openDesignComponentStatus.mockResolvedValue({ state: 'unavailable', reason: 'no manifest baked' })
    render(<CreativePage />)

    await waitFor(() => expect(creativeShow).toHaveBeenCalled())
    // Dev mode succeeded via CreativeServeWeb's checkout fallback — no
    // download card and no error banner at all.
    expect(screen.queryByText(/暂不可用/)).toBeNull()
    expect(screen.queryByRole('button', { name: '下载' })).toBeNull()
  })

  it('H1: unavailable + creativeShow reports needsDownload -> shows the not-installed download card', async () => {
    openDesignComponentStatus.mockResolvedValue({ state: 'unavailable', reason: 'no manifest baked' })
    creativeShow.mockResolvedValue({ ok: false, needsDownload: true, size: 20 * 1024 * 1024 })
    render(<CreativePage />)

    expect(await screen.findByText(/Design 需要额外下载 iDoris Design 组件（约 20 MB）/)).toBeTruthy()
    expect(screen.getByRole('button', { name: '下载' })).toBeTruthy()
  })

  it('H1: unavailable + creativeShow fails with a plain error -> shows the generic error banner, not a component card', async () => {
    openDesignComponentStatus.mockResolvedValue({ state: 'unavailable', reason: 'no manifest baked' })
    creativeShow.mockResolvedValue({ ok: false, error: 'Open Design runtime not found; set A24_OPEN_DESIGN_DIR or package resources/open-design' })
    render(<CreativePage />)

    expect(await screen.findByText(/iDoris Design 暂不可用：Open Design runtime not found/)).toBeTruthy()
    expect(screen.getByRole('button', { name: '重试' })).toBeTruthy()
    // The download card (not the generic error banner) is what owns "下载".
    expect(screen.queryByRole('button', { name: '下载' })).toBeNull()
  })

  it('drives download -> install click -> progress -> installed, then starts Creative', async () => {
    openDesignComponentStatus.mockResolvedValue({ state: 'not-installed', size: 10 * 1024 * 1024 })
    // install() only resolves once the (simulated) download/verify/extract
    // finishes — in this test, that's "never" — so the UI is driven purely
    // by the pushed progress events, same as the real main-process flow
    // where ipcMain.handle's return value and the progress push race
    // independently.
    openDesignComponentInstall.mockReturnValue(new Promise(() => {}))
    render(<CreativePage />)

    await screen.findByRole('button', { name: '下载' })
    fireEvent.click(screen.getByRole('button', { name: '下载' }))

    act(() => progressListener?.({ state: 'downloading', received: 5 * 1024 * 1024, total: 10 * 1024 * 1024 }))
    expect(await screen.findByText(/正在下载 iDoris Design 组件/)).toBeTruthy()
    expect(screen.getByText(/5 \/ 10 MB（50%）/)).toBeTruthy()

    act(() => progressListener?.({ state: 'verifying' }))
    expect(await screen.findByText('正在校验…')).toBeTruthy()

    act(() => progressListener?.({ state: 'installed', dir: '/tmp/open-design' }))
    await waitFor(() => expect(creativeShow).toHaveBeenCalled())
  })

  it('shows a retry button on install failure and retries on click', async () => {
    openDesignComponentStatus.mockResolvedValue({ state: 'not-installed', size: 1024 })
    openDesignComponentInstall
      .mockResolvedValueOnce({ state: 'failed', error: 'network error' })
      .mockResolvedValueOnce({ state: 'installed', dir: '/tmp/open-design' })
    render(<CreativePage />)

    fireEvent.click(await screen.findByRole('button', { name: '下载' }))
    expect(await screen.findByText(/iDoris Design 组件下载失败：network error/)).toBeTruthy()

    fireEvent.click(screen.getByRole('button', { name: '重试' }))
    await waitFor(() => expect(openDesignComponentInstall).toHaveBeenCalledTimes(2))
  })

  it('goes straight to Creative when already installed, with no component card shown', async () => {
    openDesignComponentStatus.mockResolvedValue({ state: 'installed', dir: '/tmp/open-design' })
    render(<CreativePage />)

    await waitFor(() => expect(creativeShow).toHaveBeenCalled())
    expect(screen.queryByText(/iDoris Design/)).toBeNull()
  })
})
