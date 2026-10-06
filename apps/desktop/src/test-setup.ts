import '@testing-library/jest-dom/vitest'

// jsdom doesn't implement scrollIntoView
if (typeof window !== 'undefined') {
  window.HTMLElement.prototype.scrollIntoView = () => {}
}

// jsdom doesn't implement ResizeObserver (used by Creative.tsx to keep the
// Open Design WebContentsView's bounds in sync with its host element).
if (typeof window !== 'undefined' && typeof window.ResizeObserver === 'undefined') {
  class NoopResizeObserver {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  }
  window.ResizeObserver = NoopResizeObserver as unknown as typeof ResizeObserver
}
