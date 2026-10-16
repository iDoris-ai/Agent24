// @vitest-environment jsdom
import { expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import ServiceBoxDemoPage from './ServiceBoxDemo'

it('renders the English service container title by default', () => {
  window.agent24 = {
    backendProxy: vi.fn(() => Promise.resolve({ ok: true, status: 200, data: { running: false, hostPort: null } })),
  } as never
  render(<ServiceBoxDemoPage />)
  expect(screen.getByText('Service container demo')).toBeInTheDocument()
})
