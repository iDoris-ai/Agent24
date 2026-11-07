// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent } from '@testing-library/react'
import { TurnRail } from './TurnRail'
import { activeTurn, buildTurns } from './turns'
import { setLocale } from '../../i18n'

describe('buildTurns', () => {
  it('makes one turn per user message, with the first reply after it', () => {
    const turns = buildTurns([
      { role: 'user', content: 'q1' },
      { role: 'assistant', content: 'a1' },
      { role: 'assistant', content: 'a1-extra' },
      { role: 'user', content: 'q2' },
      { role: 'user', content: 'q3' },
      { role: 'assistant', content: 'a3' },
    ])
    expect(turns).toEqual([
      { messageIndex: 0, prompt: 'q1', reply: 'a1' },
      { messageIndex: 3, prompt: 'q2', reply: null },
      { messageIndex: 4, prompt: 'q3', reply: 'a3' },
    ])
  })

  it('ignores an assistant message before any prompt', () => {
    expect(buildTurns([{ role: 'assistant', content: 'hi' }])).toEqual([])
  })
})

describe('activeTurn', () => {
  it('picks the last prompt above a third of the viewport', () => {
    const tops = [0, 400, 800]
    expect(activeTurn(tops, 0, 300)).toBe(0)
    expect(activeTurn(tops, 300, 300)).toBe(1)
    expect(activeTurn(tops, 750, 300)).toBe(2)
  })
})

describe('TurnRail', () => {
  beforeEach(() => { setLocale('zh') })
  const turns = [
    { messageIndex: 0, prompt: '搜索一下我支持的任务', reply: '按原表搜索，' + '共 4 项。'.repeat(30) },
    { messageIndex: 2, prompt: '先说一下要做什么', reply: null },
  ]

  it('shows a card with the prompt and a shortened reply on hover, and hides it on leave', () => {
    render(<TurnRail turns={turns} active={1} onSelect={() => {}} />)
    const marks = screen.getAllByRole('button')
    expect(marks).toHaveLength(2)
    expect(marks[1]).toHaveAttribute('aria-current', 'step')
    expect(marks[0]).not.toHaveAttribute('aria-current')

    fireEvent.mouseEnter(marks[0])
    const card = screen.getByRole('tooltip')
    expect(card).toHaveTextContent('搜索一下我支持的任务')
    expect(card.textContent).toContain('…')
    expect(card.textContent!.length).toBeLessThan(120)

    fireEvent.mouseLeave(screen.getByRole('navigation').parentElement!)
    expect(screen.queryByRole('tooltip')).toBeNull()
  })

  it('says there is no reply yet for a turn still waiting', () => {
    render(<TurnRail turns={turns} active={0} onSelect={() => {}} />)
    fireEvent.focus(screen.getAllByRole('button')[1])
    expect(screen.getByRole('tooltip')).toHaveTextContent('还没有回复')
  })

  it('selects the clicked turn', () => {
    const onSelect = vi.fn()
    render(<TurnRail turns={turns} active={0} onSelect={onSelect} />)
    fireEvent.click(screen.getByRole('button', { name: /第 2 轮/ }))
    expect(onSelect).toHaveBeenCalledWith(1)
  })
})
