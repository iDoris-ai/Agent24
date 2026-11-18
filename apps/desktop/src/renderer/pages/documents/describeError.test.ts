import { afterEach, describe, expect, it } from 'vitest'
import { setLocale } from '../../i18n'
import { describeError } from './importStore'

afterEach(() => setLocale('en'))

describe('describeError', () => {
  it('says module_delegation_required in the page language', () => {
    const res = {
      ok: false as const,
      status: 403,
      error: { code: 'module_delegation_required', message: 'Module surface requires delegation' },
    }
    expect(describeError(res)).toContain('not allowed to write this module')
    setLocale('zh')
    expect(describeError(res)).toContain('还不能写入这个模块')
    setLocale('th')
    expect(describeError(res)).toContain('ยังไม่ได้รับอนุญาตให้เขียนโมดูลนี้')
  })
})
