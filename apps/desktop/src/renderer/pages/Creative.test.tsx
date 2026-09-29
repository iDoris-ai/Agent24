// @vitest-environment jsdom
import { act, render, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import CreativePage from './Creative'

const creativeShow = vi.fn()
const creativeBounds = vi.fn()
const creativeHide = vi.fn()
let resizeCallback: ResizeObserverCallback | null = null

beforeEach(() => {
  vi.clearAllMocks()
  resizeCallback = null
  creativeShow.mockResolvedValue({ ok: true, origin: 'http://127.0.0.1:7456' })
  creativeBounds.mockResolvedValue(undefined)
  creativeHide.mockResolvedValue(undefined)
  Object.defineProperty(window, 'agent24', {
    value: { creativeShow, creativeBounds, creativeHide },
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
})
