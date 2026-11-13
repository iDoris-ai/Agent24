// @vitest-environment jsdom
import { describe, expect, it, vi, beforeEach } from 'vitest'
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { setLocale } from '../../i18n'
import SkillBar from './SkillBar'

const DIGEST = 'ffeeddccbbaa00998877665544332211ffeeddccbbaa00998877665544332211'

const rev = (id: string, kind: string, origin: string, requires?: string[]) => ({
  revision_id: id, origin, semver: '1.2.0', kind, content_digest: DIGEST, created_at: '2026-01-02T00:00:00Z',
  ...(requires ? { requires } : {}),
})

const skills = [
  { skill_id: 'ambiguous', origin: 'user', semver: '1.2.0', generation: 2, active: true },
  { skill_id: 'missing', origin: 'imported', semver: '0.2.0', generation: 0, active: false },
  { skill_id: 'steady', origin: 'user', semver: '1.0.0', generation: 1, active: true },
]

function detail(skillId: string) {
  if (skillId === 'ambiguous') {
    return {
      skill_id: skillId,
      pointer: { origin: 'user', body_revision_id: 'b2', evals_revision_id: 'e2', generation: 2 },
      generation: 2,
      revisions: [
        rev('b1', 'body', 'user', ['documents']),
        rev('e1', 'evals', 'user'),
        { ...rev('b2', 'body', 'user', ['documents']), created_at: '2026-02-01T00:00:00Z' },
        { ...rev('e2', 'evals', 'user'), created_at: '2026-02-01T00:00:00Z' },
      ],
      activations: [
        { activation_id: 'act-1', seq: 1, body_revision_id: 'b1', evals_revision_id: 'e1' },
        { activation_id: 'act-2', seq: 2, body_revision_id: 'b2', evals_revision_id: 'e2' },
      ],
      text_consistency: { consistent: 1, total: 2 },
    }
  }
  if (skillId === 'missing') {
    return {
      skill_id: skillId,
      pointer: null,
      generation: 0,
      revisions: [rev('mb', 'body', 'imported', ['model-gateway']), rev('me', 'evals', 'imported')],
      activations: [],
      text_consistency: null,
    }
  }
  return {
    skill_id: skillId,
    pointer: { origin: 'user', body_revision_id: 'sb', evals_revision_id: 'se', generation: 1 },
    generation: 1,
    revisions: [rev('sb', 'body', 'user', ['nostr-dm']), rev('se', 'evals', 'user')],
    activations: [],
    text_consistency: { consistent: 2, total: 2 },
  }
}

const components = {
  components: [
    { id: 'docs-a', state: 'ready', provides: [{ id: 'documents' }] },
    { id: 'docs-b', state: 'ready', provides: [{ id: 'documents' }] },
    { id: 'stopped', state: 'stopped', provides: [{ id: 'nostr-dm' }] },
    { id: 'dm', state: 'ready', provides: [{ id: 'nostr-dm' }] },
    { id: 'dm-down', state: 'ready', provides: [{ id: 'nostr-dm', availability: 'unavailable' }] },
  ],
}

describe('SkillBar', () => {
  const skillsActivate = vi.fn(async () => ({ ok: true, status: 200, data: {} }))
  const skillsRollback = vi.fn(async () => ({ ok: true, status: 200, data: {} }))
  const fetchSpy = vi.fn()
  const defaultProxy = async (req: { method: string; path: string }): Promise<{ ok: boolean; status: number; data: unknown }> => {
    if (req.path === '/api/v1/skills') return { ok: true, status: 200, data: { skills } }
    if (req.path === '/api/v1/components') return { ok: true, status: 200, data: components }
    const id = decodeURIComponent(req.path.replace('/api/v1/skills/', ''))
    return { ok: true, status: 200, data: detail(id) }
  }
  const proxy = vi.fn(defaultProxy)

  beforeEach(() => {
    setLocale('en')
    skillsActivate.mockClear()
    skillsRollback.mockClear()
    fetchSpy.mockClear()
    proxy.mockReset()
    proxy.mockImplementation(defaultProxy)
    skillsActivate.mockReset()
    skillsActivate.mockResolvedValue({ ok: true, status: 200, data: {} })
    vi.stubGlobal('fetch', fetchSpy)
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: proxy, skillsActivate, skillsRollback },
      writable: true,
      configurable: true,
    })
  })

  it('marks a row red when a capability is missing or has more than one provider', async () => {
    render(<SkillBar />)
    const ambiguous = await screen.findByRole('row', { name: 'ambiguous User' })
    const missing = await screen.findByRole('row', { name: 'missing Imported' })
    const steady = await screen.findByRole('row', { name: 'steady User' })
    expect(ambiguous).toHaveClass('skill-row-bad')
    expect(within(ambiguous).getByText('documents')).toHaveClass('skill-dep-bad')
    expect(missing).toHaveClass('skill-row-bad')
    expect(within(missing).getByText('model-gateway')).toHaveClass('skill-dep-bad')
    expect(steady).not.toHaveClass('skill-row-bad')
    expect(within(steady).getByText('nostr-dm')).not.toHaveClass('skill-dep-bad')
    expect(within(steady).getByText('2/2')).toBeInTheDocument()
    expect(within(steady).getByText('Manual')).toBeInTheDocument()
    expect(within(steady).getByText('User')).toBeInTheDocument()
    expect(within(missing).getByText('Imported')).toBeInTheDocument()
    expect(within(ambiguous).getByText('ffeeddcc')).toBeInTheDocument()
    expect(screen.queryByText(DIGEST)).not.toBeInTheDocument()
    expect(screen.queryByText(/base_hash/)).not.toBeInTheDocument()
    expect(screen.getByRole('columnheader', { name: 'Text consistency check' })).toBeInTheDocument()
  })

  it('activates through the main-process IPC and does not fetch the activate route', async () => {
    render(<SkillBar />)
    const steady = await screen.findByRole('row', { name: 'steady User' })
    fireEvent.click(within(steady).getByRole('button', { name: 'Activate' }))
    await waitFor(() => expect(skillsActivate).toHaveBeenCalledWith({
      skillId: 'steady', bodyRevisionId: 'sb', evalsRevisionId: 'se',
    }))
    expect(fetchSpy).not.toHaveBeenCalled()
    expect(proxy.mock.calls.some(([req]) => req.method === 'POST')).toBe(false)
    const ambiguous = await screen.findByRole('row', { name: 'ambiguous User' })
    fireEvent.click(within(ambiguous).getByRole('button', { name: 'Roll back' }))
    await waitFor(() => expect(skillsRollback).toHaveBeenCalledWith({
      skillId: 'ambiguous', rollbackActivationId: 'act-1',
    }))
  })

  const setup = (skillList: unknown[], details: Record<string, unknown>, comps = components) => {
    proxy.mockImplementation(async (req: { method: string; path: string }) => {
      if (req.path === '/api/v1/skills') return { ok: true, status: 200, data: { skills: skillList } }
      if (req.path === '/api/v1/components') return { ok: true, status: 200, data: comps }
      return { ok: true, status: 200, data: details[decodeURIComponent(req.path.replace('/api/v1/skills/', ''))] }
    })
  }
  const dualDetail = {
    skill_id: 'dual',
    pointer: { origin: 'user', body_revision_id: 'b1', evals_revision_id: 'e1', generation: 3 },
    generation: 3,
    revisions: [
      rev('b1', 'body', 'user', []), rev('e1', 'evals', 'user'),
      { ...rev('b2', 'body', 'user', []), created_at: '2026-03-01T00:00:00Z' },
      { ...rev('e2', 'evals', 'user'), created_at: '2026-03-01T00:00:00Z' },
      rev('ib', 'body', 'imported', []), rev('ie', 'evals', 'imported'),
    ],
    activations: [{ activation_id: 'a1', seq: 1, body_revision_id: 'b0', evals_revision_id: 'e0' }],
    text_consistency: null,
  }
  const dualList = [
    { skill_id: 'dual', origin: 'user', semver: '1.0.0', generation: 3, active: true },
    { skill_id: 'dual', origin: 'imported', semver: '1.0.0', generation: 3, active: true },
  ]

  // Mutation: submit the newest body and newest evals together and the two-step expectations go red.
  it('offers one side at a time when body and evals are both newer in the pointer origin', async () => {
    setup(dualList, { dual: dualDetail })
    render(<SkillBar />)
    const row = await screen.findByRole('row', { name: 'dual User' })
    expect(within(row).getByRole('button', { name: 'Activate new body' })).toBeEnabled()
    expect(within(row).getByRole('button', { name: 'Activate new evals' })).toBeEnabled()
    // A different origin than the pointer's is a whole-pair switch: one button, no rollback.
    const other = screen.getByRole('row', { name: 'dual Imported' })
    expect(within(other).getByRole('button', { name: 'Activate' })).toBeInTheDocument()
    expect(within(other).queryByRole('button', { name: 'Roll back' })).not.toBeInTheDocument()
    expect(within(row).getByRole('button', { name: 'Roll back' })).toBeInTheDocument()
  })

  // Mutation: clear busy before the reload finishes and the old step button is clickable (b1,e2).
  it('keeps the step buttons off until the reload has replaced the rows, then uses the new pointer', async () => {
    let pointer = { ...dualDetail.pointer }
    let gate: Promise<void> | null = null
    let open: () => void = () => {}
    proxy.mockImplementation(async (req: { method: string; path: string }) => {
      if (req.path === '/api/v1/skills') return { ok: true, status: 200, data: { skills: dualList } }
      if (req.path === '/api/v1/components') return { ok: true, status: 200, data: components }
      if (gate) await gate
      return { ok: true, status: 200, data: { ...dualDetail, pointer } }
    })
    render(<SkillBar />)
    const row = await screen.findByRole('row', { name: 'dual User' })
    skillsActivate.mockImplementationOnce(async () => {
      pointer = { ...pointer, body_revision_id: 'b2' }
      gate = new Promise<void>((resolve) => { open = resolve })
      return { ok: true, status: 200, data: {} }
    })
    fireEvent.click(within(row).getByRole('button', { name: 'Activate new body' }))
    await waitFor(() => expect(skillsActivate).toHaveBeenCalledWith({ skillId: 'dual', bodyRevisionId: 'b2', evalsRevisionId: 'e1' }))
    // Before the reload lands, nothing can be submitted from the old rows.
    for (const button of within(screen.getByRole('row', { name: 'dual User' })).getAllByRole('button')) expect(button).toBeDisabled()
    open()
    // After it, the row follows the new pointer: only evals are newer, one button, (b2, e2).
    const next = await within(await screen.findByRole('row', { name: 'dual User' })).findByRole('button', { name: 'Activate' })
    await waitFor(() => expect(next).toBeEnabled())
    expect(screen.queryByRole('button', { name: 'Activate new evals' })).not.toBeInTheDocument()
    fireEvent.click(next)
    await waitFor(() => expect(skillsActivate).toHaveBeenLastCalledWith({ skillId: 'dual', bodyRevisionId: 'b2', evalsRevisionId: 'e2' }))
  })

  it('disables every action and shows the error when the reload after an activation fails', async () => {
    let fail = false
    proxy.mockImplementation(async (req: { method: string; path: string }) => {
      if (fail && req.path === '/api/v1/skills') return { ok: false, status: 502, data: {} }
      if (req.path === '/api/v1/skills') return { ok: true, status: 200, data: { skills: dualList } }
      if (req.path === '/api/v1/components') return { ok: true, status: 200, data: components }
      return { ok: true, status: 200, data: dualDetail }
    })
    render(<SkillBar />)
    const row = await screen.findByRole('row', { name: 'dual User' })
    fail = true
    fireEvent.click(within(row).getByRole('button', { name: 'Activate new body' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not load the skill list')
    for (const button of within(screen.getByRole('row', { name: 'dual User' })).getAllByRole('button')) expect(button).toBeDisabled()
  })

  it('shows the IPC result: unchanged, and a localized message with the error code', async () => {
    setup(dualList, { dual: dualDetail })
    render(<SkillBar />)
    const row = await screen.findByRole('row', { name: 'dual User' })
    skillsActivate.mockResolvedValueOnce({ ok: true, status: 200, data: { result: 'unchanged', unchanged: true } })
    fireEvent.click(within(row).getByRole('button', { name: 'Activate new body' }))
    expect(await screen.findByRole('status')).toHaveTextContent('Unchanged')
    skillsActivate.mockResolvedValueOnce({ ok: false, status: 400, data: { error: { code: 'skill_activation_mixed', message: 'x' } } })
    fireEvent.click(await within(row).findByRole('button', { name: 'Activate new body' }))
    await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('skill_activation_mixed'))
    expect(screen.getByRole('status')).toHaveTextContent('Only one side')
  })

  it('says unreadable in red when the body requires cannot be read, and names missing versus ambiguous in text', async () => {
    setup(
      [{ skill_id: 'odd', origin: 'user', semver: '1.0.0', generation: 0, active: false }],
      { odd: { skill_id: 'odd', pointer: null, generation: 0, revisions: [{ ...rev('ob', 'body', 'user') }, rev('oe', 'evals', 'user')], activations: [], text_consistency: null } },
    )
    render(<SkillBar />)
    const row = await screen.findByRole('row', { name: 'odd User' })
    expect(within(row).getByText('unreadable')).toHaveClass('skill-dep-bad')
    expect(within(row).queryByText('none')).not.toBeInTheDocument()
  })

  it('marks the dependency issue with text, not only color', async () => {
    render(<SkillBar />)
    const ambiguous = await screen.findByRole('row', { name: 'ambiguous User' })
    expect(within(ambiguous).getByText(/more than one provider/)).toBeInTheDocument()
    const missing = await screen.findByRole('row', { name: 'missing Imported' })
    expect(within(missing).getByText(/\(missing\)/)).toBeInTheDocument()
  })

  it('shows an error, not the empty text, when the list cannot be loaded', async () => {
    proxy.mockImplementation(async () => ({ ok: false, status: 502, data: {} }))
    render(<SkillBar />)
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not load the skill list')
    expect(screen.queryByText('No skills installed.')).not.toBeInTheDocument()
  })
})
