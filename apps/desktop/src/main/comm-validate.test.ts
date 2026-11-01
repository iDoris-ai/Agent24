import { describe, expect, it } from 'vitest'
import { backupFileStem, isSafeBackupPassword, isSafeName } from './comm-validate'

describe('backup password rule (same as the daemon)', () => {
  it('accepts 8+ chars, up to 1024 UTF-8 bytes, one line, not blank', () => {
    expect(isSafeBackupPassword('12345678')).toBe(true)
    expect(isSafeBackupPassword('pässwörd ünï')).toBe(true)
    expect(isSafeBackupPassword('a'.repeat(1024))).toBe(true)
  })
  it.each(['short', '        ', 'a\nb-long-enough', 'a\rb-long-enough', 'a\0b-long-enough', 'a'.repeat(1025), 'é'.repeat(513)])(
    'rejects %j', (value) => expect(isSafeBackupPassword(value)).toBe(false))
})

describe('isSafeName', () => {
  it('accepts ordinary names from any script, including joiners', () => {
    for (const ok of ['alice', '张三', 'ไทย-ผู้ใช้', 'می\u200cخواهم', 'a b']) expect(isSafeName(ok)).toBe(true)
  })
  it.each([
    ['newline', 'a\nb'], ['tab', 'a\tb'], ['NUL', 'a\0b'], ['DEL', 'a\u007fb'],
    ['RLO (bidi override)', 'al\u202Eice'], ['LRE', 'al\u202Aice'], ['LRI (bidi isolate)', 'al\u2066ice'], ['PDI', 'al\u2069ice'],
    ['zero-width space', 'al\u200Bice'], ['BOM', '\uFEFFalice'], ['soft hyphen', 'al\u00ADice'], ['word joiner', 'al\u2060ice'],
    ['LS', 'a\u2028b'], ['blank', '   '], ['too long', 'x'.repeat(65)],
    ['over 512 UTF-8 bytes', '𝔘'.repeat(129)],
  ])('rejects %s', (_n, v) => expect(isSafeName(v)).toBe(false))
})

describe('isSafeName limits (the daemon contract: 128 characters, 512 bytes)', () => {
  it('uses the caller limit in characters and always caps at 512 bytes', () => {
    expect(isSafeName('x'.repeat(128), 128)).toBe(true)
    expect(isSafeName('x'.repeat(129), 128)).toBe(false)
    expect(isSafeName('𝔘'.repeat(128), 128)).toBe(true) // 128 characters, exactly 512 bytes
    expect(isSafeName('𝔘'.repeat(129), 1000)).toBe(false) // 516 bytes
  })
})

describe('backupFileStem', () => {
  it('keeps letters and digits from any script and replaces the rest', () => {
    expect(backupFileStem('alice')).toBe('alice')
    expect(backupFileStem('张三 2')).toBe('张三_2')
    expect(backupFileStem('ไทย-ผู้ใช้')).toBe('ไทย_ผู้ใช้')
    expect(backupFileStem('../../etc/passwd')).toBe('etc_passwd')
  })
  it('strips leading dots and falls back when nothing is left', () => {
    expect(backupFileStem('.hidden')).toBe('hidden')
    expect(backupFileStem('...')).toBe('agent24-identity')
    expect(backupFileStem('///')).toBe('agent24-identity')
  })
})
