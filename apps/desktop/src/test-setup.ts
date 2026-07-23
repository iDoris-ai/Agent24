import '@testing-library/jest-dom/vitest'

// jsdom doesn't implement scrollIntoView
if (typeof window !== 'undefined') {
  window.HTMLElement.prototype.scrollIntoView = () => {}
}
