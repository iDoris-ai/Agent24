// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import { render } from '@testing-library/react'
import { BrandMark } from './BrandMark'

describe('BrandMark (VI §3 菌丝节点)', () => {
  it('draws one amber core with three hyphae to three nodes, in theme colours', () => {
    const { container } = render(<BrandMark className="x" />)
    const svg = container.querySelector('svg.x')!
    expect(svg).toHaveAttribute('aria-hidden', 'true')
    expect(svg.querySelectorAll('line')).toHaveLength(3)
    const circles = [...svg.querySelectorAll('circle')]
    expect(circles).toHaveLength(4)
    expect(circles.filter(c => c.getAttribute('fill') === 'var(--accent)')).toHaveLength(1)
    expect(circles.filter(c => c.getAttribute('fill') === 'var(--text)')).toHaveLength(3)
    // The sidebar mark is small: no "24" inside the core (VI §3.2).
    expect(svg.querySelector('text')).toBeNull()
  })
})
