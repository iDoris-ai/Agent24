// @vitest-environment jsdom
import { describe, expect, it, vi, beforeEach } from 'vitest'
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { setLocale } from '../../i18n'
import { SkillPicker } from './SkillPicker'
import { notifySkillsChanged, resetConversationSkillsForTests, setConversationSkill, skillForConversation } from './session-skill'

const list = (ids: string[]) => ({ ok: true, status: 200, data: { skills: ids.map((id) => ({ skill_id: id, origin: 'user', semver: '1.0.0', generation: 1, active: true })) } })

describe('SkillPicker', () => {
  const proxy = vi.fn()
  beforeEach(() => {
    setLocale('zh')
    proxy.mockReset()
    resetConversationSkillsForTests()
    Object.defineProperty(window, 'agent24', { value: { backendProxy: proxy }, writable: true, configurable: true })
  })

  it('loads on mount, not only on focus', async () => {
    proxy.mockResolvedValue(list(['a']))
    render(<SkillPicker conversation="c1" hasMessages={false} busy={false} onStartWithSkill={vi.fn()} />)
    expect(await screen.findByRole('option', { name: 'a' })).toBeInTheDocument()
  })

  it('shows a stale choice until the list is known, then drops it with a notice and stops sending it', async () => {
    setConversationSkill('c1', 'foo')
    proxy.mockResolvedValue(list(['a']))
    render(<SkillPicker conversation="c1" hasMessages={false} busy={false} onStartWithSkill={vi.fn()} />)
    expect(screen.getByRole('combobox')).toHaveValue('foo')
    await waitFor(() => expect(screen.getByRole('combobox')).toHaveValue(''))
    expect(skillForConversation('c1')).toBeNull()
    expect(screen.getByRole('status')).toHaveTextContent('foo')
  })

  it('keeps a chosen skill visible when the list cannot be read, and shows the error', async () => {
    setConversationSkill('c1', 'foo')
    proxy.mockResolvedValue({ ok: false, status: 502, data: {} })
    render(<SkillPicker conversation="c1" hasMessages={false} busy={false} onStartWithSkill={vi.fn()} />)
    await waitFor(() => expect(screen.getByRole('alert')).toBeInTheDocument())
    expect(screen.getByRole('combobox')).toHaveValue('foo')
    expect(skillForConversation('c1')).toBe('foo')
  })

  it('does not cache an empty list or a failure: a skill activated later appears', async () => {
    proxy.mockResolvedValueOnce({ ok: false, status: 502, data: {} }).mockResolvedValue(list([]))
    render(<SkillPicker conversation="c1" hasMessages={false} busy={false} onStartWithSkill={vi.fn()} />)
    await waitFor(() => expect(screen.getByRole('alert')).toBeInTheDocument())
    fireEvent.focus(screen.getByRole('combobox'))
    await waitFor(() => expect(proxy).toHaveBeenCalledTimes(2))
    proxy.mockResolvedValue(list(['fresh']))
    act(() => notifySkillsChanged())
    expect(await screen.findByRole('option', { name: 'fresh' })).toBeInTheDocument()
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })
})
