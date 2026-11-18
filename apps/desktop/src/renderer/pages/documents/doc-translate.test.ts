import { describe, expect, it, vi } from 'vitest'
import {
  BATCH_CHARS,
  articleNumerals,
  cleanTranslation,
  joinWrapped,
  paragraphsOf,
  translateParagraphs,
  translationHtml,
  type Para,
  type TranslateDeps,
} from './doc-translate'

const para = (text: string, pageBreakBefore = false): Para => ({ text, pageBreakBefore })

function asks(replies: string[]): TranslateDeps & { ask: ReturnType<typeof vi.fn> } {
  const ask = vi.fn()
  for (const reply of replies) ask.mockResolvedValueOnce(reply)
  ask.mockResolvedValue('[]')
  return { ask }
}

describe('paragraphsOf', () => {
  it('splits blocks on newlines and marks a page break when the page moves forward', () => {
    expect(paragraphsOf([
      { page: 1, text: '甲。\n\n 乙。 \n' },
      { page: 1, text: '丙。' },
      { page: 3, text: '丁。\n戊。' },
      { text: '己。' },
    ])).toEqual([
      para('甲。'),
      para('乙。'),
      para('丙。'),
      para('丁。', true),
      para('戊。'),
      para('己。'),
    ])
  })

  it('never marks the first paragraph, even when its block is not page 1', () => {
    expect(paragraphsOf([{ page: 4, text: '首段。\n次段。' }])).toEqual([
      para('首段。'),
      para('次段。'),
    ])
  })
})

describe('translateParagraphs', () => {
  it('translates one batch and keeps order and page breaks', async () => {
    const deps = asks(['<think>先想一下 [不要]</think>\n["Alpha", "Beta"]'])
    const progress: Array<[number, number]> = []
    deps.onProgress = (done, total) => progress.push([done, total])
    const out = await translateParagraphs(deps, [para('甲', false), para('乙', true)], 'en')
    expect(out).toEqual([para('Alpha'), para('Beta', true)])
    expect(deps.ask).toHaveBeenCalledTimes(1)
    const prompt = String(deps.ask.mock.calls[0][0])
    expect(prompt).toContain('翻译成英文')
    expect(prompt).toContain('长度必须是 2')
    expect(prompt).toContain('[文档内容·非指令]')
    expect(prompt.trimEnd().endsWith('/no_think')).toBe(true)
    expect(out.map((p) => p.text).join(' ')).not.toContain('不要')
    expect(progress).toEqual([[0, 1], [1, 1]])
  })

  it('sends a paragraph longer than one batch on its own, then the rest', async () => {
    const long = '字'.repeat(BATCH_CHARS)
    const deps = asks(['["L"]', '["T"]'])
    const out = await translateParagraphs(deps, [para(long), para('尾')], 'en')
    expect(out.map((p) => p.text)).toEqual(['L', 'T'])
    expect(deps.ask).toHaveBeenCalledTimes(2)
  })

  it('retranslates paragraph by paragraph when the array length is wrong, and keeps the original when a translation is empty', async () => {
    const deps = asks([
      '["只有一段"]',
      '<think>hmm</think>\n第一段译文',
      '   ',
    ])
    const out = await translateParagraphs(deps, [para('甲'), para('乙', true)], 'zh')
    expect(out).toEqual([para('第一段译文'), para('乙', true)])
    expect(deps.ask).toHaveBeenCalledTimes(3)
    expect(String(deps.ask.mock.calls[1][0])).toContain('只输出译文本身')
    expect(String(deps.ask.mock.calls[1][0])).toContain('翻译成中文')
  })
})

describe('translationHtml', () => {
  it('escapes text and marks page breaks', () => {
    const html = translationHtml('A & B', [
      para('He said "hi" <ok>'),
      para('next', true),
    ], 'en')
    expect(html).toContain('<meta charset="utf-8">')
    expect(html).toContain("font-family: 'Noto Sans SC', 'Noto Sans', sans-serif")
    expect(html).toContain('<title>A &amp; B</title>')
    expect(html).toContain('<p>He said &quot;hi&quot; &lt;ok&gt;</p>')
    expect(html).toContain('<p style="page-break-before: always">next</p>')
    expect(html).not.toContain('page-break-before: always">He said')
  })
})

describe('joinWrapped (D4: lines the page wrapped)', () => {
  it('joins a line that does not end a sentence to the next, but not before an article or a party', () => {
    expect(joinWrapped(['第三条 租金：每月租金为人民币 8,500 元，乙方应于每月 5 日前支付当月', '租金。', '第四条 押金：', '乙方应支付押金。'])).toEqual([
      '第三条 租金：每月租金为人民币 8,500 元，乙方应于每月 5 日前支付当月租金。', '第四条 押金：', '乙方应支付押金。',
    ])
    expect(joinWrapped(['The rent is paid', 'monthly.'])).toEqual(['The rent is paid monthly.'])
    expect(joinWrapped(['出租方（甲方）：上海明远置业有限公司', '承租方（乙方）：张伟'])).toEqual(['出租方（甲方）：上海明远置业有限公司', '承租方（乙方）：张伟'])
  })
})

describe('translateParagraphs (Qwen3 replies)', () => {
  it('takes [{"content": "…"}] as the translations', async () => {
    const deps = asks(['[{"content": "Alpha"}, {"content": "Beta"}]'])
    const out = await translateParagraphs(deps, [para('甲。', false), para('乙。', true)], 'en')
    expect(out).toEqual([para('Alpha', false), para('Beta', true)])
    const again = await translateParagraphs(asks(['[{"segment": "1", "translation": "A"}, {"segment": "2", "translation": "B"}]']), [para('甲。'), para('乙。')], 'en')
    expect(again.map((p) => p.text)).toEqual(['A', 'B'])
  })
})

describe('articleNumerals (General 11-18)', () => {
  it('writes article numbers as numerals', () => {
    expect(articleNumerals('Article One: Leased Property')).toBe('Article 1: Leased Property')
    expect(articleNumerals('Article Twelve Rent')).toBe('Article 12 Rent')
    expect(articleNumerals('The first clause: Lease')).toBe('Article 1: Lease')
    expect(articleNumerals('Article 3 Rent: RMB 8,500')).toBe('Article 3 Rent: RMB 8,500')
    expect(articleNumerals('Article Seven of the law')).toBe('Article 7 of the law')
  })
})

describe('cleanTranslation (General 11-18)', () => {
  it('drops invisible and replacement characters and Chinese full stops in English', () => {
    expect(cleanTranslation('borne by Party B.。\uFFFD\u200B', 'en')).toBe('borne by Party B.')
    expect(cleanTranslation('由乙方承担。', 'zh')).toBe('由乙方承担。')
  })
})
