// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import { act, renderHook } from '@testing-library/react'
import { DEFAULT_LOCALE, LOCALE_STORAGE_KEY, defineMessages, format, getLocale, setLocale, translate, useT } from '.'

const m = defineMessages({
  en: { hi: 'Hello {name}' },
  zh: { hi: '你好 {name}' },
  th: { hi: 'สวัสดี {name}' },
})

describe('i18n', () => {
  it('ships English as the default', () => {
    expect(DEFAULT_LOCALE).toBe('en')
    expect(getLocale()).toBe('en')
  })

  it('formats placeholders and leaves unknown ones alone', () => {
    expect(format('{a} + {b}', { a: 1 })).toBe('1 + {b}')
  })

  it('persists the choice and ignores invalid locales', () => {
    setLocale('th')
    expect(window.localStorage.getItem(LOCALE_STORAGE_KEY)).toBe('th')
    setLocale('fr' as never)
    expect(getLocale()).toBe('th')
  })

  it('re-renders hook users when the locale changes', () => {
    const { result } = renderHook(() => useT(m))
    expect(result.current('hi', { name: 'A' })).toBe('Hello A')
    act(() => setLocale('zh'))
    expect(result.current('hi', { name: 'A' })).toBe('你好 A')
    expect(translate(m, 'hi', { name: 'B' }, 'th')).toBe('สวัสดี B')
  })
})
