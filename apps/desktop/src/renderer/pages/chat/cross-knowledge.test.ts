import { describe, expect, it } from 'vitest'
import { asksAcrossLibraries } from './cross-knowledge'

describe('L7-K1: 「各库」「对照标准条款」 asks that span libraries', () => {
  it.each([
    '公司制度库和项目库里，关于差旅报销的规定各是怎么说的',
    '各个知识库里关于差旅报销的规定分别是什么',
    '每个库里都有哪些关于违约金的说法',
    '所有知识库里对报销的要求有什么不同',
    '这份合同里的付款条款，对照一下我们知识库里的标准条款',
    '对照一下标准条款，看看违约金怎么写',
    '制度库与项目库的报销规定对比一下',
  ])('is one: %s', (text) => expect(asksAcrossLibraries(text)).toBe(true))

  it.each([
    '差旅报销的规定是怎么说的',
    '知识库里有没有违约金的条款',
    '项目库里的验收标准是什么',
    '各位好，帮我查一下报销流程',
    '数据库和仓库有什么区别',
    '对比一下 Python 和 Java',
    '各种库函数怎么用',
    '对照表怎么做',
    '对比一下这两份合同',
    '我不想对照标准条款，只想看第三段',
    '题库和词库各有多少题',
    '',
  ])('is not one: %s', (text) => expect(asksAcrossLibraries(text)).toBe(false))
})
