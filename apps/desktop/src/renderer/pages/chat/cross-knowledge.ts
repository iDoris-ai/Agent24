// L7-K1: 「公司制度库和项目库里…各是怎么说的」「对照一下我们知识库里的标准
// 条款」. The search already covers every library the user may use
// (knowledge_search without knowledge_base_ids), so this decides nothing about
// scope or permission: it only tells the App that an answer should speak
// library by library, so the prompt can say so. Judged on the user's own
// words by the App; a 4-bit model is not asked who 「各库」 are.

/** Words that end in 库 without being a knowledge library. */
const NOT_LIBRARY = /(?:数据|仓|图书|题|词|代码|语料|素材|曲|字|码|资源|镜像|软件|函数|类|血|金|粮|武器|档案)库/g
const LIB = '(?:知识库|[\\u4e00-\\u9fa5]{1,4}库)'
const EACH = new RegExp(`(?:各个|各自|每个|所有的?|全部|各)(?:知识)?库|(?:各个|各自|每个|所有的?|全部)${LIB}`)
const TWO_LIBS = new RegExp(`${LIB}\\s*(?:和|与|跟|、|还有)\\s*${LIB}`)
const VERB = /对照|对比|比对|比较/
const STANDARD = /知识库(?:里|中)?的?标准条款|标准条款|知识库(?:里|中)?的?(?:标准|制度|规定|条款)/
const REFUSED = /(?:不要|别|不用|先别|不想|不必|不)\s*(?:再)?\s*(?:对比|比较|比对|对照)/
const TWO_DOCS = /这两份|两份(?:文档|文件|合同)?|这两个(?:文档|文件)/

export function asksAcrossLibraries(text: string): boolean {
  const said = text.trim().replace(NOT_LIBRARY, '')
  if (!said || REFUSED.test(said)) return false
  if (EACH.test(said) && /库/.test(said)) return true
  if (TWO_LIBS.test(said)) return true
  if (VERB.test(said) && STANDARD.test(said) && !TWO_DOCS.test(said)) return true
  return false
}
