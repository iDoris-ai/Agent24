import { describe, expect, it } from 'vitest'
import { asksTranslation, docAgentPrompt, extractionFields, findByTitle, parseDocIntent, subjectOf, translatesDocument, wantsDocTool } from './doc-intent'

describe('D2b document intents in a conversation about a document', () => {
  it('reads page ranges to split out', () => {
    expect(parseDocIntent('把第2到4页拆出来')).toEqual({ kind: 'split', pages: '2-4' })
    expect(parseDocIntent('拆出第 2-4 页')).toEqual({ kind: 'split', pages: '2-4' })
    expect(parseDocIntent('把第三页单独拆出来')).toEqual({ kind: 'split', pages: '3' })
    expect(parseDocIntent('提取第1页和第5到七页成新文档')).toEqual({ kind: 'split', pages: '1,5-7' })
    expect(parseDocIntent('拆分出第十二页')).toEqual({ kind: 'split', pages: '12' })
  })

  it('leaves ordinary questions to the model', () => {
    expect(parseDocIntent('第2页说了什么？')).toBeNull()
    expect(parseDocIntent('付款期限是多少')).toBeNull()
    expect(parseDocIntent('这个条款怎么拆解理解？')).toBeNull()
  })

  it('asks how to say it when the pages or the other document are missing', () => {
    expect(parseDocIntent('拆分一下')).toEqual({ kind: 'unclear', about: 'split' })
    expect(parseDocIntent('帮我把它拆开')).toEqual({ kind: 'unclear', about: 'split' })
    expect(parseDocIntent('把这两份合并')).toEqual({ kind: 'unclear', about: 'merge' })
    expect(parseDocIntent('把第4到2页拆出来')).toEqual({ kind: 'unclear', about: 'split' })
  })

  it('reads a merge with a named document', () => {
    expect(parseDocIntent('和《保险单 B》合并')).toEqual({ kind: 'merge', other: '保险单 B' })
    expect(parseDocIntent('把它跟「boil-water」合在一起')).toEqual({ kind: 'merge', other: 'boil-water' })
  })

  it('finds the named document by title, the original over what was made from it', () => {
    const docs = [
      { document_id: 'doc_1', title: '保险单 B.pdf' },
      { document_id: 'doc_2', title: 'boil-water.pdf' },
      { document_id: 'doc_3', title: '合并：CFYAD26.pdf + boil-water.pdf' },
      { document_id: 'doc_4', title: 'notice A.pdf' },
      { document_id: 'doc_5', title: 'notice B.pdf' },
    ]
    expect(findByTitle(docs, '保险单 B')).toEqual({ match: docs[0] })
    expect(findByTitle(docs, 'boil-water')).toEqual({ match: docs[1] })
    expect(findByTitle(docs, 'boil')).toEqual({ match: docs[1] })
    expect(findByTitle(docs, 'notice')).toEqual({ candidates: [docs[3], docs[4]] })
    expect(findByTitle(docs, '不存在')).toBeNull()
  })
})

const HINT = '需要处理文档时请使用 documents 工具（documents.list 查找；documents.compose 每调用一次生成一份新 PDF：合并是一次调用带多个 parts，拆成 N 份是调用 N 次、每次只带一个 part；'
  + '按模板生成：先 documents.fields 查看模板里的 {{字段}}，再用 documents.fill 填写，values 的键是不带花括号的字段名，值取自用户给的信息；'
  + '改写 Word 段落：先 documents.paragraphs 查看段落编号，再用 documents.patch 改写（每段给 index、新文字和 anchor，anchor 必须照抄这一段开头至少 12 个字，少于 12 个字会被拒；编号可以不写，写了要对），tracked=true 写成修订；document_id 可以直接填文件名；compose、fill、patch 成功一次就结束，不要重复调用。回答用户时不要提这些工具名）。'

describe('docAgentPrompt', () => {
  it('names the current document when there is one, then the tools hint and the user text', () => {
    expect(docAgentPrompt({ id: 'doc_1', title: '保单.pdf' }, '拆出第 2 页')).toBe(
      `当前对话的文档：《保单.pdf》（document_id: doc_1）。\n${HINT}\n拆出第 2 页`,
    )
  })

  it('adds only the tools hint when there is no current document', () => {
    expect(docAgentPrompt(null, '把两份合并')).toBe(
      `${HINT}\n把两份合并`,
    )
  })
})

describe('subjectOf (without the Agent, outside a document conversation)', () => {
  it('is the first title named, unless it is the other of a merge', () => {
    const split = parseDocIntent('把《CFYAD26-9770136.pdf》拆出第 1 页')!
    expect(split).toEqual({ kind: 'split', pages: '1' })
    expect(subjectOf('把《CFYAD26-9770136.pdf》拆出第 1 页', split)).toBe('CFYAD26-9770136.pdf')
    const merge = parseDocIntent('把《A》和《B》合并')!
    expect(subjectOf('把《A》和《B》合并', merge)).toBe('A')
    const onlyOther = parseDocIntent('和《B》合并')!
    expect(subjectOf('和《B》合并', onlyOther)).toBeNull()
    expect(subjectOf('拆出第 1 页', split)).toBeNull()
  })
})

describe('findByTitle with the extension named (General 11-18)', () => {
  const docs = [
    { document_id: 'a', title: '中文测试合同.pdf', created_at: '2026-11-18T01:00:00Z' },
    { document_id: 'b', title: '中文测试合同.pdf', created_at: '2026-11-18T03:00:00Z' },
    { document_id: 'c', title: '中文测试合同.docx', created_at: '2026-11-18T02:00:00Z' },
  ]
  it('takes the newest of the same full title, and says how many there were', () => {
    expect(findByTitle(docs, '中文测试合同.pdf')).toEqual({ match: docs[1], among: 2 })
    expect(findByTitle(docs, '中文测试合同.docx')).toEqual({ match: docs[2] })
  })
})

describe('parseDocIntent: questions about a page are not splits (PR-Daemon #1909 ①)', () => {
  it.each([
    '帮我提取第2页的关键信息', '第3页单独说了什么', '请取出第5页的金额', '导出第2页说的要点',
    // A document named with its file name is not a word for making a PDF (General 11-19).
    '帮我提取《CFYAD26-9770136.pdf》第2页的关键信息', '帮我提取 CFYAD26-9770136.pdf 第2页的关键信息',
    '《拆分方案.docx》第2页说了什么',
    // PR-Daemon #1915: 「文档/文件」 in a question, and questions themselves.
    '这份文档第2页的关键信息帮我提取一下', '提取第2页的文件内容',
    '第3页导出的金额是多少，文档里写了吗', '总结一下这份文件第4页，单独列出金额',
    '能把第2页拆出来吗？',
  ])('%s → null', (text) => {
    expect(parseDocIntent(text)).toBeNull()
  })
  it.each([
    ['把第 2 页提取成新文档', '2'],
    ['把第3页单独存成一份 PDF', '3'],
    ['取出第5页另存', '5'],
    ['拆出第 2-4 页', '2-4'],
    ['把《合同.pdf》第2页提取成新文档', '2'],
    ['把第2页单独存成一份', '2'],
  ])('%s → split %s', (text, pages) => {
    expect(parseDocIntent(text)).toEqual({ kind: 'split', pages })
  })
})

describe('M1: the acceptance sentences reach the documents tools or the translation', () => {
  // Loop 5 M1 acceptance, as said in a conversation about a document.
  it('split', () => expect(wantsDocTool('把这份 PDF 拆成第 1 页一份、其余一份', true)).toBe(true))
  it('merge', () => expect(wantsDocTool('把这两份合成一份', true)).toBe(true))
  it('fill a template', () => expect(wantsDocTool('用这些信息填这份录用通知书', true)).toBe(true))
  it('rewrite a paragraph', () => expect(wantsDocTool('把第 3 段改得正式一点', true)).toBe(true))
  it('translate and export Word', () => {
    expect(asksTranslation('把这份翻成英文并导出 Word', true)).toBe(true)
    expect(translatesDocument('把这份翻成英文并导出 Word', true)).toBe(true)
    expect(wantsDocTool('把这份翻成英文并导出 Word', true)).toBe(false)
  })
  it('a knowledge question is neither', () => {
    expect(wantsDocTool('烧开水要多久', false)).toBe(false)
    expect(asksTranslation('烧开水要多久', false)).toBe(false)
  })
  it('a document named in 《》 counts as one outside its conversation', () => {
    expect(wantsDocTool('把《合同.docx》第 3 段改得正式一点', false)).toBe(true)
    expect(wantsDocTool('用这些信息填《录用通知书.docx》', false)).toBe(true)
  })
})

describe('M1: everyday sentences without a document stay ordinary chat', () => {
  for (const text of [
    '把这句话改成更礼貌的说法',
    '帮我把这段话改得口语一点',
    '这封邮件润色一下',
    '把“你好”翻成英文',
    '填这个表要注意什么',
    // General/PM 11-21: everyday questions with 填/改/翻译 in them.
    '帮我翻译一下这句话',
    '这个表怎么填比较好',
    '翻译成英文是什么意思',
    '这段代码怎么改成异步的',
  ]) {
    it(text, () => {
      expect(wantsDocTool(text, false)).toBe(false)
      expect(translatesDocument(text, false)).toBe(false)
      expect(parseDocIntent(text)).toBeNull()
    })
  }
})

describe('M1: the four questions PR-Daemon #1915 found taken as splits stay questions', () => {
  // #1915 review ①: asked in a conversation about the document, so with it.
  for (const text of [
    '这份文档第2页的关键信息帮我提取一下',
    '提取第2页的文件内容',
    '第3页导出的金额是多少，文档里写了吗',
    '总结一下这份文件第4页，单独列出金额',
  ]) {
    it(text, () => {
      expect(parseDocIntent(text)).toBeNull()
      expect(wantsDocTool(text, true)).toBe(false)
      expect(translatesDocument(text, true)).toBe(false)
    })
  }
})

describe('F3: one sentence asks for fields of the document', () => {
  it.each([
    ['把这份合同的甲方、乙方、金额、期限抽出来', ['甲方', '乙方', '金额', '期限']],
    ['这张发票的开票日期和总金额是多少，列出来', ['开票日期', '总金额']],
    ['从这份通知里找出截止日期和要交的材料', ['截止日期', '要交的材料']],
    ['把《租赁合同.pdf》的租金、押金抽出来', ['租金', '押金']],
  ])('%s', (text, fields) => {
    expect(extractionFields(text, true)).toEqual(fields)
    expect(parseDocIntent(text)).toBeNull()
  })

  it('names a document in 《》 outside its conversation', () => {
    expect(extractionFields('把《租赁合同.pdf》的租金、押金抽出来', false)).toEqual(['租金', '押金'])
  })

  it('asks to extract without fields the rules can read', () => {
    expect(extractionFields('抽取一下', true)).toEqual([])
  })
})

describe('F3: ordinary questions and other tasks are not extractions', () => {
  for (const [text, inDocument] of [
    ['这个字段怎么填', true],
    ['甲方是谁？', true],
    ['付款期限是多少', true],
    ['这份合同有什么风险', true],
    ['找出原因', true],
    ['把第3页拆出来', true],
    ['把这份合同翻译成英文', true],
    ['总结一下这份文件第4页，单独列出金额', true],
    ['这份文档第2页的关键信息帮我提取一下', true],
    ['把第 3 段改得正式一点', true],
    ['列出来几个周末去处', false],
    ['帮我找出这段代码的日期处理问题', false],
    ['今天天气怎么样', false],
    // PR-Daemon #2019 ①: more than an extraction, a question, or no field named.
    ['把重要日期都列出来提醒我', true],
    ['把第3条抽出来单独看看', true],
    ['列出来所有的风险点', true],
    ['把这份合同里的问题列出来', true],
    ['你能列出来这份文档的大纲吗', true],
    ['把《年度报告.pdf》里的数据抽出来做个图', true],
    ['提取出来的内容有问题吗', true],
    ['把关键信息都抽出来', true],
    ['你好', false],
  ] as const) {
    it(text, () => expect(extractionFields(text, inDocument)).toBeNull())
  }
})
