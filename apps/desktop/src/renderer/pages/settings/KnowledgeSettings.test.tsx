// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import { KnowledgeSettings, parseKnowledgeBaseIds } from './KnowledgeSettings'

const status = vi.fn()
const configure = vi.fn()
const setEnabled = vi.fn()

beforeEach(() => {
  vi.clearAllMocks()
  status.mockResolvedValue({ enabled: false, endpoint: 'https://kb.example/api/v1', knowledgeBaseIds: ['kb-a'], hasKey: true, active: false })
  configure.mockResolvedValue({ ok: true })
  setEnabled.mockResolvedValue({ ok: true })
  Object.defineProperty(window, 'agent24', {
    value: { knowledge: { status, configure, setEnabled } },
    writable: true,
    configurable: true,
  })
})

describe('KnowledgeSettings', () => {
  it('splits knowledge base ids on lines and commas', () => {
    expect(parseKnowledgeBaseIds(' kb-a,\nkb-b , ,\n')).toEqual(['kb-a', 'kb-b'])
  })

  it('shows the saved settings, never a key, and keeps the key when the field is left empty', async () => {
    render(<KnowledgeSettings />)
    expect(await screen.findByDisplayValue('https://kb.example/api/v1')).toBeInTheDocument()
    expect(screen.getByLabelText('Retrieve key')).toHaveValue('')
    expect(screen.getByText('A key is saved. Leave empty to keep it.')).toBeInTheDocument()
    fireEvent.change(screen.getByLabelText('Knowledge base IDs'), { target: { value: 'kb-a\nkb-b' } })
    fireEvent.click(screen.getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(configure).toHaveBeenCalledWith({ endpoint: 'https://kb.example/api/v1', knowledgeBaseIds: ['kb-a', 'kb-b'] }))
    expect(await screen.findByText('Saved.')).toBeInTheDocument()
  })

  it('sends a new key once and clears the field', async () => {
    render(<KnowledgeSettings />)
    await screen.findByDisplayValue('https://kb.example/api/v1')
    fireEvent.change(screen.getByLabelText('Retrieve key'), { target: { value: 'sk-1' } })
    fireEvent.click(screen.getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(configure).toHaveBeenCalledWith(expect.objectContaining({ apiKey: 'sk-1' })))
    await waitFor(() => expect(screen.getByLabelText('Retrieve key')).toHaveValue(''))
  })

  it('switches Knowledge on and shows why a save was refused', async () => {
    configure.mockResolvedValue({ ok: false, reason: 'invalid' })
    render(<KnowledgeSettings />)
    await screen.findByDisplayValue('https://kb.example/api/v1')
    fireEvent.click(screen.getByLabelText('Let the assistant use Knowledge'))
    await waitFor(() => expect(setEnabled).toHaveBeenCalledWith(true))
    fireEvent.click(screen.getByRole('button', { name: 'Save' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('Check the address')
  })
})

// K2: the last check of the server, red while it does not answer; 「Check now」 asks again.
describe('KnowledgeSettings: the server check (K2)', () => {
  it('shows a server that cannot be reached as an alert, and clears it after a check finds it back', async () => {
    const on = { enabled: true, endpoint: 'http://127.0.0.1:18282/api/v1', knowledgeBaseIds: ['kb-a'], hasKey: true, active: true }
    status.mockResolvedValue({ ...on, reachability: { state: 'unreachable', checkedAt: '2026-11-19T08:00:00Z' } })
    const check = vi.fn().mockResolvedValue({ ...on, reachability: { state: 'ok', checkedAt: '2026-11-19T08:01:00Z' } })
    ;(window.agent24.knowledge as unknown as Record<string, unknown>).check = check
    render(<KnowledgeSettings />)
    expect(await screen.findByRole('alert')).toHaveTextContent('Cannot reach the knowledge server')
    fireEvent.click(screen.getByRole('button', { name: 'Check now' }))
    await waitFor(() => expect(screen.queryByText(/Cannot reach the knowledge server/)).not.toBeInTheDocument())
    expect(screen.getByText(/The knowledge server answers/)).toBeInTheDocument()
  })
})
