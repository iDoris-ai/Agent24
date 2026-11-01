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
    fireEvent.click(screen.getByLabelText('Use Knowledge in the main chat'))
    await waitFor(() => expect(setEnabled).toHaveBeenCalledWith(true))
    fireEvent.click(screen.getByRole('button', { name: 'Save' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('Check the address')
  })
})
