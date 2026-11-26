import { describe, expect, it } from 'vitest'
import { asksDeleteDocument, asksDeleteParagraph, asksFillForm, asksTranslation, continuesFormFill, docAgentPrompt, FORM_HINT, extractionFields, formClaimUnbacked, formMaterial, materialHint, findByTitle, parseDocIntent, subjectOf, translatesDocument, wantsDocTool } from './doc-intent'

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
  + '按模板生成：先 documents.fields 查看模板里的 {{字段}}，再用 documents.fill 填写，values 的键是不带花括号的字段名，值取自用户给的信息，这些字段名都放进 user_said；'
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

  it.each([
    ['把合同金额抽取出来', ['合同金额']],
    ['把金额、日期抽出来，谢谢', ['金额', '日期']],
    ['列出来甲方和乙方', ['甲方', '乙方']],
    ['请把发票号码、开票日期、金额抽出来', ['发票号码', '开票日期', '金额']],
    ['甲方和乙方分别是谁，列出来', ['甲方', '乙方']],
    ['把租金、押金、开户银行抽出来', ['租金', '押金', '开户银行']],
    ['把发票号码和结算日期抽出来', ['发票号码', '结算日期']],
  ])('PR-Daemon #2019: %s', (text, fields) => {
    expect(extractionFields(text, true)).toEqual(fields)
  })

  it('names a document in 《》 outside its conversation', () => {
    expect(extractionFields('把《租赁合同.pdf》的租金、押金抽出来', false)).toEqual(['租金', '押金'])
  })

  it('asks to extract without fields the rules can read', () => {
    expect(extractionFields('抽取一下', true)).toEqual([])
  })
})

// PR-Daemon #2019 ①: whatever is said after the verb asks for more than an
// extraction, so every pairing of these fields and these follow-ups goes to the model.
describe('F3: an extraction followed by something more goes to the model', () => {
  const asks = [
    ['把重要日期都列出来', '，到时候提醒我'], ['把日期列出来', '记到日历里'], ['把金额都列出来', '算个总数'],
    ['把联系人抽出来', '存到通讯录'], ['把甲方信息抽出来', '发邮件给老王'], ['把付款日期列出来', '，我好安排资金'],
    ['把金额列出来', '按大小排序'], ['把截止日期找出来', '加到待办'], ['把材料清单列出来', '打印'],
    ['把金额抽出来', '换算成美元'], ['把联系人、电话抽出来', '备用'], ['列出来甲方和乙方', '，然后发给我'],
  ] as const
  for (const [ask, then] of asks) {
    it(ask + then, () => {
      expect(extractionFields(ask + then, true)).toBeNull()
      expect(extractionFields(ask, true)).not.toBeNull()
    })
  }
  // Round 3: the verb first, then something more.
  const finds = ['找出截止日期', '找出金额', '找出合同里的金额', '列出来金额', '找出电话号码'] as const
  const more = ['并设个提醒', '并核对是否正确', '后告诉老板', '以便安排', '发给我', '加起来', '写错了没有', '并计算总和',
    // Round 4: follow-ups that themselves end in a field word.
    '并发到我的邮箱', '告诉负责人', '做成清单', '并核对合同金额', '之后算总价', '提醒我交材料', '并发给对方公司'] as const
  for (const ask of finds) {
    for (const then of more) {
      it(ask + then, () => expect(extractionFields(ask + then, true)).toBeNull())
    }
    it(`${ask} alone is an extraction`, () => expect(extractionFields(ask, true)).not.toBeNull())
  }
  // Round 3: what is judged, not stated, is not a field.
  for (const text of ['把合同的漏洞列出来', '把所有不合理的条款列出来', '列出来这份合同里的优缺点', '列出来三个改进建议',
    '把这份文件的结构列出来', '把合同的亮点列出来', '把作者的观点列出来', '列出来这份文件里提到的人',
    '找出与上一版不同的金额', '列出来甲方和乙方的区别',
    '列出来你推荐的付款方式', '把对我不利的条件列出来', '把有问题的金额列出来', '找出不合理的费用', '找出前后不一致的金额']) {
    it(text, () => expect(extractionFields(text, true)).toBeNull())
  }
  for (const text of ['把这份合同里需要注意的地方列出来', '把这份合同的第三部分抽出来', '把里面需要我签字的地方列出来']) {
    it(text, () => expect(extractionFields(text, true)).toBeNull())
  }
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

describe('L6-F2: deleting a whole document, and what is not', () => {
  it.each([
    ['把这份删了', true],
    ['删除这个文件', true],
    ['删掉这份文档吧', true],
  ])('in a document conversation, %s asks to delete it', (text, inDocument) => {
    expect(asksDeleteDocument(text, inDocument)).toBe(true)
    expect(wantsDocTool(text, inDocument)).toBe(true)
    expect(docAgentPrompt(null, text)).toContain('documents.delete')
  })
  it.each(['删除上一份拆出来的文件', '把《中文测试合同.pdf》删了', '帮我把那份 PDF 删除'])('%s names a document to delete', (text) => {
    expect(asksDeleteDocument(text, false)).toBe(true)
    expect(wantsDocTool(text, false)).toBe(true)
  })
  // The plan's counter-examples: a part of a document is a rewrite, never a delete.
  it.each([
    ['删掉第 3 段', true],
    ['删掉这句话', true],
    ['把这份里的第 3 段删掉', true],
    ['把第三条删除', true],
    ['把《合同.docx》里的这一行删掉', false],
    ['删除第 2 页', true],
  ])('%s is not deleting the document', (text, inDocument) => {
    expect(asksDeleteDocument(text, inDocument)).toBe(false)
    expect(docAgentPrompt(null, text)).not.toContain('documents.delete')
  })
  it.each([
    '今天心情不错', '我把那个 app 删了，好用的推荐一个', '删了吧，不聊这个了', '怎么删除文档？', '删除的文件还能恢复吗',
    '把这份 PDF 拆成第 1 页一份、其余一份',
  ])('chat, a question or a split: %s', (text) => {
    expect(asksDeleteDocument(text, false)).toBe(false)
  })
  // PR-Daemon #2028 round 2: a title is not words of the sentence.
  it.each(['把《都市报告》删了', '把《不要的草稿》删了', '把《别人的合同》删掉', '删除《年度汇总（所有部门）》', '删掉这份吧，都过期了', '删了吧'])('%s deletes the one document', (text) => {
    expect(asksDeleteDocument(text, true)).toBe(true)
  })
  // PR-Daemon #2028 round 3: every way to name the document × a part of it is
  // not a delete of the document; neither is a delete narrowed or taken back.
  const objects = ['这份', '这个文件', '上一份', '上一份拆出来的文件', '刚才拆出来的', '刚才生成的', '《》']
  const parts = ['的第3页', '里的页码', '的封面', '的水印', '中的末页', '的第2页']
  it.each(objects.flatMap((o) => parts.flatMap((p) => [`删掉${o.replace('《》', '《合同》')}${p}`, `把${o.replace('《》', '《合同》')}${p}删了`])))('round 3: %s', (text) => {
    expect(asksDeleteDocument(text, true)).toBe(false)
    expect(asksDeleteDocument(text, false)).toBe(false)
  })
  it.each([
    '把上一份和刚才生成的都删了', '删掉这份，还有那份', '删了吧，只保留第3页', '删了，第3页别动', '删了吧，不过要保留封面',
    '删了吧，不聊这个了', '删掉这份，第3页留着', '删了吧，算了', '把这份删了，不对',
  ])('round 3: %s is not deleting one whole document', (text) => {
    expect(asksDeleteDocument(text, true)).toBe(false)
  })
  it.each([
    '不删了', '先不删了', '甭删了', '算了不删了', '删掉末页', '删掉首页', '删掉空白页', '删掉页码', '删掉第3到5页', '删掉前面两页',
    '删掉这两页', '删掉附录', '删掉标题', '删掉脚注', '删掉插图', '删掉公章', '删除超链接', '把日期删了', '删掉第二章', '删掉错别字',
    '这几份文件删了', '删除这两份文件', '删掉PDF的末页', '删掉这个文件的页码',
  ])('round 2: %s is not deleting one whole document', (text) => {
    expect(asksDeleteDocument(text, true)).toBe(false)
    expect(asksDeleteDocument(text, false)).toBe(false)
  })
  // PR-Daemon #2028 ①: not to delete, many documents, or a part of one.
  it.each([
    '先别删掉这份', '不用删了', '不要删除这份文档', '删除所有文件', '把这些文件全部删除',
    '删掉最后一页', '删掉前两页', '删除最后两页', '删掉页眉', '删掉表格', '把封面删了', '把多余的空行删了', '移除密码', '删掉签名',
  ])('%s is not deleting one whole document', (text) => {
    expect(asksDeleteDocument(text, true)).toBe(false)
    expect(asksDeleteDocument(text, false)).toBe(false)
    expect(docAgentPrompt(null, text)).not.toContain('documents.delete')
  })
})

/** Only the paragraph hint says this. */
const PARAGRAPH_HINT_PART = '不是删除整份文档；先用 documents.paragraphs 核对那一段'

describe('L7-K0 (FU-184): taking a numbered part out of a document is a rewrite', () => {
  it.each([
    ['把《合同-真实结构.docx》里的第 3 段删掉', false],
    ['把《合同.docx》第3段删除', false],
    ['删除第 2 段', true],
    ['第三条去掉', true],
    ['把第 5 句删了吧', true],
    // PR-Daemon #2055: a limit after the delete, or a title with a negation in it.
    ['删掉第3段，其他不要动', true],
    ['把第3段删掉，不要改别的', true],
    ['删除第 3 段，不用保留', true],
    ['把第3段删掉，不必解释', true],
    ['把《不对的合同.docx》第3段删掉', false],
    ['把《不要动.docx》里的第3段删掉', false],
    ['把《不能说的秘密.docx》的第3段删掉', false],
  ])('%s → documents tools', (text, inDocument) => {
    expect(asksDeleteParagraph(text, inDocument)).toBe(true)
    expect(wantsDocTool(text, inDocument)).toBe(true)
    // A rewrite, never the whole-document delete card.
    expect(asksDeleteDocument(text, inDocument)).toBe(false)
    // The hint that says so is this function's own, not the base one's.
    expect(docAgentPrompt({ id: 'doc_x', title: 't' }, text)).toContain(PARAGRAPH_HINT_PART)
  })
  it.each([
    // Everyday questions and talk about something else stay with the knowledge bases.
    ['烧开水要多久', false],
    ['第 3 条建议删掉哪个比较好', false],
    ['第 3 段怎么删', true],
    ['《合同.docx》里第 3 段删掉会怎样？', false],
    ['把第 3 个杯子洗了', false],
    ['删除一份文件需要多久', false],
    ['这份合同的违约金是多少', true],
    ['把《合同.pdf》删了', false],
    ['第 3 段删掉算了', true],
    ['第3段不需要删', true],
    ['不许删第3段', true],
    ['禁止删除第3段', true],
    // PR-Daemon #2048: told not to, or asking, inside a document's conversation.
    ['不要删第 3 段', true],
    ['别删第3段', true],
    ['先别删掉第 3 段', true],
    ['第3段不要删', true],
    ['第 3 段不用删', true],
    ['第 3 条不删了', true],
    ['不要把第 3 段删掉', true],
    ['第3段要不要删', true],
    ['第 3 条建议删掉哪个比较好', true],
    ['删掉第3段会不会影响合同效力', true],
    ['第3段删掉会怎样', true],
    ['《合同.docx》第3段不要删', false],
  ])('%s → not a paragraph delete', (text, inDocument) => {
    expect(asksDeleteParagraph(text, inDocument)).toBe(false)
    // Neither sent to the Agent with the opposite hint nor given it.
    expect(docAgentPrompt({ id: 'doc_x', title: 't' }, text)).not.toContain(PARAGRAPH_HINT_PART)
  })
})

describe('L8-S02: filling a form', () => {
  it.each([
    ['把这张入职登记表填一下，资料在《个人信息.docx》里', false],
    ['这张表能填吗？', false],
    ['用我的资料把这张表填了', false],
    ['按《入职登记表模板.docx》填写', false],
    ['帮我填一下', true],
  ])('%s is a form fill (in a document: %s)', (text, inDocument) => {
    expect(asksFillForm(text, inDocument)).toBe(true)
    expect(wantsDocTool(text, inDocument)).toBe(true)
  })

  it.each([
    ['烧开水要多久', false],
    ['这份合同的甲方是谁', true],
    ['帮我填一下', false],
  ])('%s is not', (text, inDocument) => {
    expect(asksFillForm(text, inDocument)).toBe(false)
  })

  it('a form turn carries the form hint: support, identity, manual fields, one by one', () => {
    const prompt = docAgentPrompt(null, '把这张入职登记表填一下，资料在《个人信息.docx》里')
    expect(prompt).toContain(FORM_HINT)
    expect(prompt.endsWith('\n把这张入职登记表填一下，资料在《个人信息.docx》里')).toBe(true)
    for (const said of ['source_document_id', 'user_said', 'ask_user', 'from_source', '不用自己读资料', '一律不从资料里取', '岗位字段', '空字符串', '不要自己编', '手工处理', 'PDF', '打印后手填', '逐项']) {
      expect(FORM_HINT).toContain(said)
    }
    expect(docAgentPrompt(null, '把两份合并')).not.toContain(FORM_HINT)
  })

  it('an answer right after a form turn is part of it: it reaches the tools with the hint', () => {
    const before = ['把这张入职登记表填一下，资料在《个人信息.docx》里']
    expect(continuesFormFill(before, '电话是 13912345678')).toBe(true)
    expect(continuesFormFill(before, '紧急联系人那栏我还没想好，先空着')).toBe(true)
    expect(docAgentPrompt(null, '电话是 13912345678', true)).toContain(FORM_HINT)
    // Up to three turns back, never further, and never without a form turn.
    expect(continuesFormFill([...before, '电话 139', '地址北京', '身份证 110'], '好了吗')).toBe(false)
    expect(continuesFormFill(['烧开水要多久'], '电话是 13912345678')).toBe(false)
    expect(continuesFormFill([], '电话是 13912345678')).toBe(false)
  })
})

describe('L8-S02 真机 0.7.122: a form turn that says it made a document it did not', () => {
  const prompt = docAgentPrompt(null, '没有其他了，紧急联系人留空，签名我自己手写，直接生成吧', true)
  const said = '已生成《入职登记表》最终版，文档已保存为《入职登记表模板（填写）》。'

  it('reads as unchanged when the run wrote nothing', () => {
    expect(formClaimUnbacked(prompt, said, null)).toEqual({ attempted: true, jobs: [], failed: [] })
    expect(formClaimUnbacked(prompt, said, undefined)).toEqual({ attempted: true, jobs: [], failed: [] })
    for (const s of ['表已经填好了。', '已保存到文档页。', '生成好了，请查看。', '已为你创建新文档。']) {
      expect(formClaimUnbacked(prompt, s, null)).not.toBeNull()
    }
  })

  it('leaves a write the run did, a question, and a turn that is not a form alone', () => {
    const wrote = { attempted: true, jobs: ['job_1'], failed: [] }
    expect(formClaimUnbacked(prompt, said, wrote)).toBe(wrote)
    expect(formClaimUnbacked(prompt, '请告诉我你的姓名、电话和身份证号。', null)).toBeNull()
    expect(formClaimUnbacked(docAgentPrompt(null, '这份合同讲了什么', false), said, null)).toBeNull()
  })

  it('tells the model a confirmed fill must call documents.fill', () => {
    expect(FORM_HINT).toContain('必须调用 documents.fill')
  })
})

describe('监工 0.7.123: the material a form fill was told', () => {
  it('is the title after 资料 in this turn or the last three', () => {
    expect(formMaterial([], '把《入职登记表模板.docx》填一下，资料在《个人信息.docx》里')).toBe('个人信息.docx')
    expect(formMaterial(['把这张表填一下，资料是《个人信息.docx》', '电话 139'], '确认无误，生成吧')).toBe('个人信息.docx')
    expect(formMaterial([], '把《入职登记表模板.docx》填一下')).toBeNull()
    expect(formMaterial(['资料在《旧.docx》里', 'a', 'b', 'c'], '生成吧')).toBeNull()
    // PR-Daemon #2100 B3: told not to use it is not the material.
    for (const no of ['资料不要用《李四.docx》，部门先空着', '资料别用《李四.docx》', '资料没有《李四.docx》']) {
      expect(formMaterial([], no)).toBeNull()
    }
    // r2 C3 (监工): only 「资料在《X》(里|中)」「资料是《X》」「用《X》(里|中)的资料」; any
    // other 「资料…《X》」 in this turn is none, with no fall back to earlier turns.
    expect(formMaterial([], '用《个人信息.docx》里的资料')).toBe('个人信息.docx')
    expect(formMaterial([], '资料在《个人信息.docx》中')).toBe('个人信息.docx')
    for (const no of ['资料勿用《X.docx》', '资料无需《X.docx》', '资料请忽略《X.docx》', '资料莫用《X.docx》', '资料非《X.docx》', '资料用《X.docx》']) {
      expect(formMaterial([], no)).toBeNull()
    }
    expect(formMaterial(['资料是《李四.docx》'], '资料别用《李四.docx》')).toBeNull()
    // r3 D2/D3 (监工): any word of no or of taking it back in the turn: none, no fall back.
    for (const no of ['别用《李四.docx》里的资料', '不要用《李四.docx》里的资料，部门先空着', '请勿用《李四.docx》中的资料', '别说资料是《李四.docx》',
      '资料是《李四.docx》，这份别用，改用《王小明.docx》里的资料']) {
      expect(formMaterial([], no)).toBeNull()
    }
    expect(formMaterial(['资料是《李四.docx》'], '刚才那份资料不要用了')).toBeNull()
    // r4 E1–E3 (监工): a clause the whole of which names it, no word list.
    for (const no of ['禁用《李四.docx》里的资料', '资料是《李四.docx》（作废）', '禁止用《李四.docx》里的资料', '取消用《李四.docx》里的资料']) {
      expect(formMaterial([], no)).toBeNull()
    }
    expect(formMaterial(['资料是《李四.docx》'], '那份不用了')).toBeNull()
    expect(formMaterial([], '资料是《莫小明.docx》')).toBe('莫小明.docx')
    expect(formMaterial([], '资料在《个人信息.docx》里，部门没有的先空着')).toBe('个人信息.docx')
    expect(formMaterial([], '把《入职登记表模板.docx》这张表填一下，资料在《个人信息.docx》里')).toBe('个人信息.docx')
  })

  it('goes into the prompt by its id, for fields and fill', () => {
    const hint = materialHint({ id: 'doc_01M8035XWKZJGGD3NHF3X0HJ0J', title: '个人信息.docx', revision: 1 })
    expect(hint).toContain('《个人信息.docx》')
    expect(hint).toContain('source_document_id=doc_01M8035XWKZJGGD3NHF3X0HJ0J')
    expect(hint).toContain('documents.fill')
    const prompt = docAgentPrompt(null, '生成吧', true, hint)
    expect(prompt).toContain(hint)
    expect(prompt.endsWith('\n生成吧')).toBe(true)
    expect(docAgentPrompt(null, '生成吧', true)).not.toContain('source_document_id=doc_')
  })
})
