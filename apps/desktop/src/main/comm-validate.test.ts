import { describe, expect, it } from 'vitest'
import { backupFileStem, isSafeBackupPassword } from './comm-validate'

describe('backup password rule (same as the daemon)', () => {
  it('accepts 8+ chars, up to 1024 UTF-8 bytes, one line, not blank', () => {
    expect(isSafeBackupPassword('12345678')).toBe(true)
    expect(isSafeBackupPassword('pässwörd ünï')).toBe(true)
    expect(isSafeBackupPassword('a'.repeat(1024))).toBe(true)
  })
  it.each(['short', '        ', 'a\nb-long-enough', 'a\rb-long-enough', 'a\0b-long-enough', 'a'.repeat(1025), 'é'.repeat(513)])(
    'rejects %j', (value) => expect(isSafeBackupPassword(value)).toBe(false))
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
