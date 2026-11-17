// @vitest-environment jsdom
import { render } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { Markdown } from './Markdown'

describe('Markdown (assistant replies)', () => {
  it('renders bold, lists, headings and code as elements, not asterisks', () => {
    const { container } = render(<Markdown text={'## 保单要点\n\n**保险期间**：一年\n\n- 第一项 *重要*\n- 第二项 `A-1`\n\n1. 甲\n2. 乙\n\n```\nraw **x**\n```'} />)
    expect(container.querySelector('.md-h2')?.textContent).toBe('保单要点')
    expect(container.querySelector('strong')?.textContent).toBe('保险期间')
    expect(container.querySelectorAll('ul li')).toHaveLength(2)
    expect(container.querySelector('em')?.textContent).toBe('重要')
    expect(container.querySelector('li code')?.textContent).toBe('A-1')
    expect(container.querySelectorAll('ol li')).toHaveLength(2)
    expect(container.querySelector('pre')?.textContent).toBe('raw **x**')
    expect(container.textContent).not.toContain('**保险期间**')
  })

  it('never turns text into markup', () => {
    const { container } = render(<Markdown text={'<img src=x onerror=alert(1)> **ok**'} />)
    expect(container.querySelector('img')).toBeNull()
    expect(container.textContent).toContain('<img src=x onerror=alert(1)>')
  })
})
