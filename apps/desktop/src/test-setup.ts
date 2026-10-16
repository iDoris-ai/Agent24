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

// UI language: every test starts from the shipped default (English) with
// nothing persisted; suites that assert another language opt in themselves.
if (typeof window !== 'undefined') {
  const { beforeEach } = await import('vitest')
  const { DEFAULT_LOCALE, LOCALE_STORAGE_KEY, setLocale } = await import('./renderer/i18n')
  beforeEach(() => {
    setLocale(DEFAULT_LOCALE)
    window.localStorage.removeItem(LOCALE_STORAGE_KEY)
  })
}
