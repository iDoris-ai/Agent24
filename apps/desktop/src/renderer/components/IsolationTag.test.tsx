// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import { act, cleanup, render, screen } from '@testing-library/react'
import { setLocale } from '../i18n'
import { IsolationTag } from './IsolationTag'

describe('IsolationTag', () => {
  it('renders the isolated and not-isolated labels, and nothing when isolation is missing', () => {
    cleanup()
    setLocale('en')
    const { rerender } = render(<IsolationTag isolation={{ state: 'isolated', mechanism: 'seatbelt' }} />)
    expect(screen.getByText('Isolated')).toBeInTheDocument()

    rerender(<IsolationTag isolation={{ state: 'not_isolated', reason: 'platform_unsupported' }} />)
    expect(screen.getByText('Not isolated · Isolation is not available on this platform')).toBeInTheDocument()

    rerender(<IsolationTag isolation={{ state: 'not_isolated', reason: 'form_not_covered' }} />)
    expect(screen.getByText('Not isolated · This kind of component is not isolated')).toBeInTheDocument()

    rerender(<IsolationTag isolation={undefined} />)
    expect(screen.queryByText(/Isolated|Not isolated/)).not.toBeInTheDocument()

    act(() => setLocale('zh'))
    rerender(<IsolationTag isolation={{ state: 'isolated', mechanism: 'seatbelt' }} />)
    expect(screen.getByText('已隔离')).toBeInTheDocument()
    rerender(<IsolationTag isolation={{ state: 'not_isolated', reason: 'platform_unsupported' }} />)
    expect(screen.getByText('未隔离 · 此平台暂不支持隔离')).toBeInTheDocument()
    rerender(<IsolationTag isolation={{ state: 'not_isolated', reason: 'form_not_covered' }} />)
    expect(screen.getByText('未隔离 · 此类组件暂不隔离')).toBeInTheDocument()

    act(() => setLocale('th'))
    rerender(<IsolationTag isolation={{ state: 'isolated', mechanism: 'seatbelt' }} />)
    expect(screen.getByText('แยกส่วนแล้ว')).toBeInTheDocument()
    rerender(<IsolationTag isolation={{ state: 'not_isolated', reason: 'platform_unsupported' }} />)
    expect(screen.getByText('ยังไม่แยกส่วน · แพลตฟอร์มนี้ยังไม่รองรับการแยกส่วน')).toBeInTheDocument()
    rerender(<IsolationTag isolation={{ state: 'not_isolated', reason: 'form_not_covered' }} />)
    expect(screen.getByText('ยังไม่แยกส่วน · ส่วนประกอบประเภทนี้ยังไม่ถูกแยกส่วน')).toBeInTheDocument()

    act(() => setLocale('en'))
  })
})
