import { describe, expect, it } from 'vitest'
import { l1, l2 } from './text'

describe('l1', () => {
  it('folds compatibility forms, as NFKC does', () => {
    expect(l1('Water shut-oﬀ')).toBe('Water shut-off')
    expect(l1('１２：００（周一）')).toBe('12:00(周一)')
  })

  it('makes each run of whitespace, line breaks too, one space', () => {
    expect(l1('  from 9:00\n to  17:00\t')).toBe('from 9:00 to 17:00')
    expect(l1('端午　节')).toBe('端午 节')
  })
})

describe('l2', () => {
  it('drops a space with a CJK character on either side', () => {
    expect(l2('B 座')).toBe('B座')
    expect(l2('沪财税〔2025〕145 号')).toBe('沪财税〔2025〕145号')
    expect(l2('2025 年 12 月 22 日')).toBe('2025年12月22日')
    expect(l2('周六、 上班\n端午节')).toBe('周六、上班端午节')
    expect(l2('あ い')).toBe('あい')
  })

  it('knows the ideographs beyond the basic plane', () => {
    expect(l2('\u{20000} \u{2A6DF}')).toBe('\u{20000}\u{2A6DF}')
    expect(l2('B \u{20000} B')).toBe('B\u{20000}B')
  })

  it('changes nothing when applied again', () => {
    for (const text of ['B 座 0302–2208 单元', '  Water  shut-oﬀ\n10 月 13 日 ', '周六、 上班']) {
      expect(l2(l2(text))).toBe(l2(text))
      expect(l1(l1(text))).toBe(l1(text))
    }
  })

  it('keeps the spaces between words of scripts that use them', () => {
    expect(l2('Water  shut-off 12 October')).toBe('Water shut-off 12 October')
    expect(l2('한국 어')).toBe('한국 어')
  })

  it('gives the same form to a gold quote and to the text layer that broke it', () => {
    expect(l2('物业服务中心')).toBe(l2('物业服务 \n中 心'))
  })
})
