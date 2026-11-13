import { describe, expect, it } from 'vitest'
import { packageIntegrityHint } from './packageIntegrity'

describe('packageIntegrityHint', () => {
  it.each([
    ['integrity_manifest_missing', '包版本过旧，升级 Sin90/Cos72 后重试'],
    ['hash_mismatch', '文件校验失败，请重装可信版本'],
    ['integrity_manifest_invalid', '安装内容不完整或格式不合法，请重装'],
    ['signature_rejected', '发布签名验证失败，请安装可信版本'],
    ['integrity_check_failed', '无法校验包，请检查访问权限后重试'],
  ])('shows the fixed prompt for %s', (reason, prompt) => {
    expect(packageIntegrityHint(reason)).toBe(prompt)
  })

  it('does not invent a prompt for another reason', () => {
    expect(packageIntegrityHint('gave_up')).toBeUndefined()
    expect(packageIntegrityHint(null)).toBeUndefined()
  })
})
