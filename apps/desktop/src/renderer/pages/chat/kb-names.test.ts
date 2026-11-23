import { describe, expect, it } from 'vitest'
import { libraryLabel, parseKbNames, formatKbNames } from './kb-names'

describe('L7-K1: library names the user gave', () => {
  it('reads 「ID=名称」 lines, skips blanks and malformed lines, keeps the last of a repeat', () => {
    expect(parseKbNames('kb-a=制度库\n\n kb-b = 项目库 \nnonsense\n=无ID\nkb-a=制度库2')).toEqual({ 'kb-a': '制度库2', 'kb-b': '项目库' })
    expect(formatKbNames({ 'kb-a': '制度库' })).toBe('kb-a=制度库')
  })
  it('labels a bound library by its name, or 「知识库 N」 by binding order; never shows an id', () => {
    const bound = ['kb-a', 'kb-b']
    expect(libraryLabel('kb-a', bound, { 'kb-a': '制度库' }, '知识库 {n}')).toBe('制度库')
    expect(libraryLabel('kb-b', bound, { 'kb-a': '制度库' }, '知识库 {n}')).toBe('知识库 2')
  })
  it('a name given for a library that is not bound is not used, and an unbound id gets the plain label', () => {
    expect(libraryLabel('kb-x', ['kb-a'], { 'kb-x': '别人的库' }, '知识库 {n}')).toBe('知识库')
  })
  it('strips control and bidi characters and cuts a long name', () => {
    expect(parseKbNames('kb-a=制‮度\n库')).toEqual({ 'kb-a': '制度' })
    expect(parseKbNames(`kb-a=${'长'.repeat(80)}`)['kb-a']).toHaveLength(40)
  })
})
