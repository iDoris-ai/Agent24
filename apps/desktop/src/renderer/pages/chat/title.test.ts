import { describe, expect, it } from 'vitest'
import { promptTitle, renameTitle } from './title'

describe('conversation titles (UI-04)', () => {
  it('takes the first prompt on one line, cut to 60 characters', () => {
    expect(promptTitle('  周报\n\n 草稿  ')).toBe('周报 草稿')
    expect(promptTitle('字'.repeat(60))).toBe('字'.repeat(60))
    expect(promptTitle('字'.repeat(61))).toBe(`${'字'.repeat(60)}…`)
    expect(promptTitle('')).toBe('')
  })

  it('never splits an emoji or a letter with its accent', () => {
    const thumbs = '👍🏽' // four UTF-16 units, one character
    expect(promptTitle(`${'a'.repeat(59)}${thumbs}tail`)).toBe(`${'a'.repeat(59)}${thumbs}…`)
    expect(promptTitle(`${'a'.repeat(59)}éx`)).toBe(`${'a'.repeat(59)}é…`)
  })

  it('keeps a rename to 80 characters on one line, without an ellipsis', () => {
    expect(renameTitle(' 新\t名字 ')).toBe('新 名字')
    expect(renameTitle('字'.repeat(81))).toBe('字'.repeat(80))
    expect(renameTitle(`${'字'.repeat(79)} 尾`)).toBe(`${'字'.repeat(79)}`)
    expect(renameTitle(' \n ')).toBe('')
  })
})
