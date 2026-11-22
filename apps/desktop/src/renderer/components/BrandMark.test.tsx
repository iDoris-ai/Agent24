// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import { render } from '@testing-library/react'
import { BrandMark } from './BrandMark'

describe('BrandMark (VI §3 昼夜环)', () => {
  it('draws 24 hour ticks, exactly one lit, plus the core dot, in theme colours', () => {
    const { container } = render(<BrandMark className="x" />)
    const svg = container.querySelector('svg.x')!
    expect(svg).toHaveAttribute('aria-hidden', 'true')
    const ticks = svg.querySelectorAll('line')
    expect(ticks).toHaveLength(24)
    const lit = [...ticks].filter(t => t.getAttribute('stroke') === 'var(--accent)')
    expect(lit).toHaveLength(1)
    expect(svg.querySelector('circle')).toHaveAttribute('fill', 'var(--text)')
  })
})
