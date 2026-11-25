// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import { act, cleanup, render, screen } from '@testing-library/react'
import { setLocale } from '../i18n'
import { PrivacyBadge } from './PrivacyBadge'

describe('PrivacyBadge', () => {
  it('marks local and remote with a word and a shape, not colour alone (VI §4.2)', () => {
    cleanup()
    act(() => setLocale('zh'))
    const { container, rerender } = render(<PrivacyBadge tier="local" />)
    const local = screen.getByText('本地').closest('.privacy-badge')!
    expect(local).toHaveClass('privacy-badge-local')
    expect(local).toHaveTextContent('●')
    expect(local).toHaveAttribute('title', '在本机处理，没有离开这台电脑')

    rerender(<PrivacyBadge tier="remote" />)
    const remote = screen.getByText('远端').closest('.privacy-badge')!
    expect(remote).toHaveClass('privacy-badge-remote')
    expect(remote).toHaveTextContent('↗')
    expect(remote).toHaveAttribute('title', '发往远端模型处理')

    // Unknown or missing tier: say nothing rather than guess.
    rerender(<PrivacyBadge tier={null} />)
    expect(container).toBeEmptyDOMElement()
    rerender(<PrivacyBadge tier="lora" />)
    expect(container).toBeEmptyDOMElement()

    act(() => setLocale('en'))
    rerender(<PrivacyBadge tier="remote" />)
    expect(screen.getByText('Remote')).toBeInTheDocument()
  })
})
