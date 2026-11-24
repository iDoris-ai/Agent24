// L7-K2: onboarding is at most three questions in the main chat. The App
// matches the words and calls main; the model is not asked.

import type { KnowledgeOnboardPreflight, KnowledgeOnboardResult, KnowledgeUpload } from '../../../shared/ipc-types'

const QUESTION_END = /(?:了吗|没有)[。！!？?\s]*$/
const REFUSAL = /(?:不(?:用|要|想|必|需要)?|没必要|无需|没有必要)\s*(?:再)?\s*开通|(?:^|[^特])别\s*开通/
const OPEN = /(?:开通|启用).{0,16}知识库|知识库.{0,16}(?:开通|启用)/
const BUILD_KB = /建(?:立)?(?:一个|个)?知识库/
// 「建个库」 only with something to put in it. 「建个数据库」 is not that 库.
const BUILD_SHORT = /建(?:立)?(?:一个|个)库(?!数据)/
const SHORT_ABOUT = /合同|文档|资料|知识|放进去/
// A whole answer that stops onboarding. Not a substring or a prefix: 「合同取消条款审核」「取消订单的处理流程」
// are purposes, not a cancel.
const CANCEL = /^(?:算了|取消|先不用了?|先不开通了?|不开通了?|不用了|不要了|我不想开通了|算了不弄了|取消取消)(?:吧|了)?(?:不开通了?|开通|谢谢)?吧?$/
/** Bare refusals at the purpose and types questions also stop onboarding. */
const BARE_NO = /^(?:不|不用|不要|不需要|没有|没|不了)$/

const PURPOSE_QUESTION = '好的，我来帮你开通知识库。先问三个问题。\n1. 这个知识库主要做什么用？（例如：合同审核、公司制度）'
const TYPES_QUESTION = '主要会放哪些类型的文档？（目前只支持 Markdown（.md）和纯文本（.txt），PDF、Word 暂不支持）'
const LOCAL_QUESTION = '3. 这些资料是否必须只留在这台电脑上、不能放到服务器？请回答“是”（只能留在本机）或“否”（可以放到服务器）。'
const LOCAL_AGAIN = '没能确定你的意思。请只回答“是”（资料只能留在本机）或“否”（可以放到服务器）。'
const CANCEL_REPLY = '好的，先不开通。'
const UNCLEAR_REPLY = '没听明白，先不开通了；需要时再说“帮我开通知识库”。'

const ALREADY_BOUND = '你已经连着知识库了。开通只用于还没有连知识库的情况，这次不会改动你现在的知识库设置；要换库请到“设置 → 知识库”里修改。'
const INVALID_ACTIVATION = '这台电脑上的激活信息无效，开通不了，请联系提供激活的人重新激活。'
const NOT_ACTIVATED = '这台电脑还没有激活知识库服务，暂时不能开通，请先完成激活。'
const FORMAT_LINE = '现在可以上传文档：目前支持 .md 和 .txt，单个不超过 10 MB；PDF、Word 暂不支持。'
const OFFICE_NOTE = '你提到的 PDF/Word 等格式这一版还不能上传。'

export type OnboardStep = {
  step: 'purpose' | 'types' | 'local'
  purpose?: string
  types?: string
  askedLocalAgain?: boolean
}

export function asksOnboarding(text: string): boolean {
  const said = text.trim()
  if (!said || QUESTION_END.test(said) || REFUSAL.test(said)) return false
  if (OPEN.test(said) || BUILD_KB.test(said)) return true
  return BUILD_SHORT.test(said) && SHORT_ABOUT.test(said)
}

export function asksUpload(text: string): boolean {
  const said = text.trim()
  if (!said) return false
  if (/上传/.test(said) && /知识库/.test(said)) return true
  if (/传[到至]知识库/.test(said)) return true
  if (/往知识库.{0,8}加/.test(said) && /文档|文件|资料/.test(said)) return true
  if (/知识库(?:里|中).{0,6}加(?:文档|文件|资料)/.test(said)) return true
  return false
}

/** 做 / 用于 / 用来 / 主要 plus some content, not the request to open itself. */
function purposeOf(text: string): string | null {
  const patterns = [
    /主要做([^，,。！？?\n]{1,80})/,
    /主要用于([^，,。！？?\n]{1,80})/,
    /主要用来([^，,。！？?\n]{1,80})/,
    /主要是([^，,。！？?\n]{1,80})/,
    /用于([^，,。！？?\n]{1,80})/,
    /用来([^，,。！？?\n]{1,80})/,
    /主要([^，,。！？?\n]{1,80})/,
    /做([^，,。！？?\n]{1,80})/,
  ]
  for (const pattern of patterns) {
    const found = pattern.exec(text)?.[1]
    if (!found) continue
    const purpose = cleanPurpose(found)
    if (purpose) return purpose
  }
  return null
}

function cleanPurpose(raw: string): string | null {
  let s = raw.trim()
  s = s.replace(/(?:[，,]\s*)?(?:请|帮我|帮忙|给我|把)?(?:开通一下|开通|启用).*$/, '')
  s = s.replace(/(?:[，,]\s*)?(?:请|帮我|帮忙|给我)?建(?:立)?(?:一个|个)?库.*$/, '')
  s = s.replace(/[的呢吧啊呀\s]+$/g, '').trim()
  if ([...s].length < 2) return null
  return s
}

/** The first question, or the types question when the opening sentence already says what it is for. */
export function beginOnboard(text: string): { state: OnboardStep; reply: string } {
  const purpose = purposeOf(text)
  if (purpose) {
    return {
      state: { step: 'types', purpose },
      reply: `好的，我来帮你开通知识库。按你说的，用来「${purpose}」。\n${TYPES_QUESTION}`,
    }
  }
  return { state: { step: 'purpose' }, reply: PURPOSE_QUESTION }
}

/**
 * The answer to the privacy question. A consent gate: anything not clearly one or the other is asked again, and
 * a negated server phrase («不能放到服务器», the question's own words) means the material stays local.
 */
const NO_ANSWERS = new Set([
  // Only the 否 the question defines, or a sentence that names the server. Not 不 / 不用 (stop at the earlier
  // questions, or an echo of 「不能放到服务器」), and not 都行 / 没关系 / 无所谓 / 不是 / 可以, which may only mean
  // «no preference» or a misheard choice: those are asked again (QA 11-25).
  '否',
  '可以放服务器', '可以放到服务器', '可以放在服务器', '可以放到服务器上', '可以放在服务器上',
  '能放服务器', '能放到服务器', '放服务器也行', '放服务器也可以', '放到服务器也行', '放到服务器也可以',
])

function localChoice(text: string): 'yes' | 'no' | null {
  if (/[？?]|吗|会不会|安全不|行不行/.test(text)) return null
  const s = text.replace(/[。！!，,、；;：:\s]/g, '')
  if (!s) return null
  // «不能/不要/没必要/别/不 … 服务器» or «不能上传»: must stay on this computer. Any negator, so that a negated
  // server phrase can never be read as consent.
  const negator = '(?:不能|不要|不可以|不可|不允许|不准|不必|不需要|不用|没必要|没有必要|无需|无须|别|不)'
  if (new RegExp(`${negator}(?:放|存|传|上传|放到|放在|存到|传到)?.{0,6}(?:服务器|云端|网上|外面)`).test(s)
    || new RegExp(`${negator}上传`).test(s)) return 'yes'
  if (/^(?:是|是的|对|对的|必须|必须的|要|需要)(?:$|[^不].*$)/.test(s) && !/不是|不用|不必|不需要/.test(s)) return 'yes'
  if (/^(?:只在本机|只留在本机|只留在这台电脑上?|只放本地|只能在本机|只能放本地)/.test(s)) return 'yes'
  // Consent to the server is a closed list of whole answers; anything else is asked again.
  // Only trailing punctuation is dropped here: 「不，是」 is a self-correction, not 「不是」.
  if (NO_ANSWERS.has(text.trim().replace(/[。！!，,、；;\s]+$/, ''))) return 'no'
  return null
}

export function nextOnboard(state: OnboardStep, answer: string): {
  state: OnboardStep | null
  reply: string
  run?: { localOnly: boolean }
  cancelled?: boolean
} {
  const said = answer.trim()
  const bare = said.replace(/[。！!，,、\s]/g, '')
  if (CANCEL.test(bare)) return { state: null, reply: CANCEL_REPLY, cancelled: true }
  if (state.step !== 'local' && BARE_NO.test(bare)) return { state: null, reply: CANCEL_REPLY, cancelled: true }
  if (state.step === 'purpose') return { state: { step: 'types', purpose: said }, reply: TYPES_QUESTION }
  if (state.step === 'types') {
    return { state: { step: 'local', purpose: state.purpose, types: said }, reply: LOCAL_QUESTION }
  }
  const choice = localChoice(said)
  if (choice !== null) return { state: null, reply: '', run: { localOnly: choice === 'yes' } }
  if (state.askedLocalAgain) return { state: null, reply: UNCLEAR_REPLY, cancelled: true }
  return {
    state: { step: 'local', purpose: state.purpose, types: state.types, askedLocalAgain: true },
    reply: LOCAL_AGAIN,
  }
}

export function preflightReply(p: KnowledgeOnboardPreflight): string | null {
  if (p.alreadyBound) return ALREADY_BOUND
  if (p.invalidActivation) return INVALID_ACTIVATION
  if (!p.activated) return NOT_ACTIVATED
  if (p.reachable === false) return '暂时连不上知识库服务，这次没有开通，稍后再试。'
  return null
}

function mentionsOffice(types: string): boolean {
  return /表格|扫描|(?:^|[^a-z])(?:pdf|word|docx?|pptx?|excel)(?![a-z])/i.test(types)
}

export function resultReply(r: KnowledgeOnboardResult, typesAnswer?: string): string {
  if (r.ok) {
    const lead = r.outcome === 'created'
      ? '已在知识库服务器上为你建好默认知识库，之后上传的文档都存放在服务器上。'
      : '你在知识库服务器上的默认知识库之前已经建好了，没有再建新的；上传的文档存放在服务器上。'
    const note = r.outcome === 'created' && typesAnswer && mentionsOffice(typesAnswer) ? OFFICE_NOTE : ''
    // Saved, but the local service did not take it now (not up, or refused): say so without promising when.
    const later = r.activeNow ? '' : '设置已保存，但本机服务这会儿还没接上这个知识库；如果提问时没有用上知识库，请到“设置 → 知识库”看一下。'
    return `${lead}${FORMAT_LINE}${note}${later}`
  }
  switch (r.code) {
    case 'local_only_unavailable':
      return '目前只提供放在服务器上的知识库，只在本机的版本还没有，这次没有开通。'
    case 'already_bound':
      return ALREADY_BOUND
    case 'not_activated':
      return NOT_ACTIVATED
    case 'activation_invalid':
      return INVALID_ACTIVATION
    case 'in_progress':
      return '另一次开通还在进行，请稍等一会儿再说一次“帮我开通知识库”。'
    case 'not_offered':
      return '知识库服务暂不提供开通，这次没有开通。'
    case 'unreachable':
      return '没连上知识库服务，这次可能没有开通成功。服务器不会留下半个库，稍后再说一次“帮我开通知识库”即可，不会重复建库。'
    case 'server_error':
    case 'unavailable':
      return '开通没有成功（服务出错），没有留下半个库，稍后再试。'
    case 'not_saved':
      return '知识库已在服务器上建好，但这台电脑没能保存设置，请再说一次“帮我开通知识库”。'
    default:
      return '开通没有成功（服务出错），没有留下半个库，稍后再试。'
  }
}

function refuseReason(reason: string | null): string {
  if (reason === 'unsupported_format') return '这种格式暂不支持（只支持 .md、.txt）'
  if (reason === 'too_large') return '超过 10 MB'
  if (reason === 'not_a_file') return '不是文件'
  if (reason === 'not_connected') return '还没有连上知识库'
  return `服务器拒绝了（${reason ?? ''}）`
}

export function uploadLine(u: KnowledgeUpload): string {
  const name = `《${u.fileName}》`
  let line: string
  switch (u.state) {
    case 'refused':
      line = `${name}没有上传：${refuseReason(u.reason)}`
      break
    case 'sending':
      line = `${name}正在上传`
      break
    case 'saved':
      line = `${name}已保存，等待处理`
      break
    case 'processing':
      line = `${name}处理中`
      break
    case 'indexed':
      line = `${name}已可检索${u.enrichment === 'running' ? '，摘要还在生成' : ''}`
      break
    case 'failed':
      line = `${name}处理失败（${u.reason ?? ''}）`
      break
    case 'cancelled':
      line = `${name}已取消。${u.originalRetained ? '原件仍保存在服务器上；' : ''}取消前已发生的处理照实计费`
      break
    default:
      line = u.reason === 'unreachable'
        ? `${name}上传后没有收到可用的答复，不确定是否已上传成功`
        : `${name}状态不明`
  }
  if (!u.costFinal) {
    // A job that has stopped but whose cost is not final yet (W5.10): say so; main keeps asking until it is.
    const settled = u.state === 'cancelled' || u.state === 'failed' || (u.state === 'indexed' && u.enrichment !== 'running')
    return u.jobId !== null && settled ? `${line}，费用暂未结算` : line
  }
  if (u.costZero) return `${line}，没有产生费用`
  if (u.costUnknown) return `${line}，费用未知`
  // Final, not all exact zero, nothing unknown: the amounts are the server's (estimated items included).
  return `${line}，费用以服务器记录为准`
}
