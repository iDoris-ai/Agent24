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

  it('L7-K1: keeps library names on this machine only; configure never sees them', async () => {
    localStorage.removeItem('a24.knowledge.libraryNames')
    render(<KnowledgeSettings />)
    await screen.findByDisplayValue('https://kb.example/api/v1')
    fireEvent.change(screen.getByLabelText('Library names (optional)'), { target: { value: 'kb-a=制度库' } })
    fireEvent.click(screen.getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(configure).toHaveBeenCalledWith({ endpoint: 'https://kb.example/api/v1', knowledgeBaseIds: ['kb-a'] }))
    expect(localStorage.getItem('a24.knowledge.libraryNames')).toBe('kb-a=制度库')
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

// Loop 9 3c: remote or the local Lite, one at a time; the Lite's install and launch in one line.
describe('KnowledgeSettings: the local knowledge base (Loop 9 3c)', () => {
  const setSource = vi.fn()
  const liteRetry = vi.fn()
  const remote = { enabled: true, endpoint: 'https://kb.example/api/v1', knowledgeBaseIds: ['kb-a'], hasKey: true, active: true, source: 'remote', activeKnowledgeBaseIds: ['kb-a'], local: { phase: 'off' } }
  beforeEach(() => {
    setSource.mockResolvedValue({ ok: true })
    liteRetry.mockResolvedValue(undefined)
    Object.defineProperty(window, 'agent24', {
      value: { knowledge: { status, configure, setEnabled, setSource, liteRetry } },
      writable: true,
      configurable: true,
    })
  })

  it('offers the local knowledge base and switches to it; the remote form goes away while local', async () => {
    status.mockResolvedValue(remote)
    render(<KnowledgeSettings />)
    await screen.findByDisplayValue('https://kb.example/api/v1')
    status.mockResolvedValue({ ...remote, source: 'local', active: false, activeKnowledgeBaseIds: [], local: { phase: 'downloading', received: 41_943_040, total: 83_886_080 } })
    fireEvent.click(screen.getByLabelText('Local knowledge base (lite)'))
    await waitFor(() => expect(setSource).toHaveBeenCalledWith('local'))
    expect(await screen.findByText(/Downloading… 40\.0 \/ 80\.0 MB/)).toBeInTheDocument()
    expect(screen.queryByLabelText('WeKnora API address')).toBeNull()
    expect(screen.getByText('On, but the local knowledge base is not ready yet.')).toBeInTheDocument()
  })

  it('says why it did not start, with a retry; never the words about upgrading', async () => {
    status.mockResolvedValue({ ...remote, source: 'local', active: false, local: { phase: 'failed', reason: 'embedding_unavailable' } })
    render(<KnowledgeSettings />)
    expect(await screen.findByText(/the local model service \(iDoris\) is not ready/)).toBeInTheDocument()
    status.mockResolvedValue({ ...remote, source: 'local', local: { phase: 'ready' } })
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }))
    await waitFor(() => expect(liteRetry).toHaveBeenCalled())
    expect(await screen.findByText('Ready. Uploads and searches stay on this computer.')).toBeInTheDocument()
    expect(document.body.textContent ?? '').not.toMatch(/upgrad/i)
  })

  it('a failed download says so and offers a retry', async () => {
    status.mockResolvedValue({ ...remote, source: 'local', active: false, local: { phase: 'install_failed', failure: 'network' } })
    render(<KnowledgeSettings />)
    expect(await screen.findByRole('alert')).toHaveTextContent('The download failed. Check the network, then retry.')
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }))
    await waitFor(() => expect(liteRetry).toHaveBeenCalled())
  })

  it('switching back to remote shows the saved server again', async () => {
    status.mockResolvedValue({ ...remote, source: 'local', local: { phase: 'ready' } })
    render(<KnowledgeSettings />)
    await screen.findByText('Ready. Uploads and searches stay on this computer.')
    status.mockResolvedValue(remote)
    fireEvent.click(screen.getByLabelText('Remote knowledge base (server)'))
    await waitFor(() => expect(setSource).toHaveBeenCalledWith('remote'))
    expect(await screen.findByLabelText('WeKnora API address')).toHaveValue('https://kb.example/api/v1')
  })
})
