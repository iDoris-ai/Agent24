import { describe, expect, it } from 'vitest'
import type { KnowledgeOnboardPreflight, KnowledgeOnboardResult, KnowledgeUpload } from '../../../shared/ipc-types'
import {
  asksOnboarding, asksUpload, beginOnboard, nextOnboard, preflightReply, resultReply, uploadLine, type OnboardStep,
} from './knowledge-onboard'

const PURPOSE = '好的，我来帮你开通知识库。先问三个问题。\n1. 这个知识库主要做什么用？（例如：合同审核、公司制度）'
const TYPES = '主要会放哪些类型的文档？（目前只支持 Markdown（.md）和纯文本（.txt），PDF、Word 暂不支持）'
const LOCAL = '3. 这些资料是否必须只留在这台电脑上、不能放到服务器？请回答“是”（只能留在本机）或“否”（可以放到服务器）。'
const AGAIN = '没能确定你的意思。请只回答“是”（资料只能留在本机）或“否”（可以放到服务器）。'
const CANCEL = '好的，先不开通。'
const UNCLEAR = '没听明白，先不开通了；需要时再说“帮我开通知识库”。'
const FORMAT = '现在可以上传文档：目前支持 .md 和 .txt，单个不超过 10 MB；PDF、Word 暂不支持。'
const OFFICE = '你提到的 PDF/Word 等格式这一版还不能上传。'
const BOUND = '你已经连着知识库了。开通只用于还没有连知识库的情况，这次不会改动你现在的知识库设置；要换库请到“设置 → 知识库”里修改。'

function ready(over: Partial<KnowledgeOnboardPreflight> = {}): KnowledgeOnboardPreflight {
  return {
    activated: true,
    invalidActivation: false,
    alreadyBound: false,
    reachable: true,
    formats: ['md', 'markdown', 'txt'],
    maxBytes: 10_485_760,
    ...over,
  }
}

function up(over: Partial<KnowledgeUpload> & Pick<KnowledgeUpload, 'state'>): KnowledgeUpload {
  return {
    uploadId: 'u1',
    fileName: '制度.md',
    reason: null,
    jobId: null,
    enrichment: null,
    costFinal: false,
    costZero: false,
    costUnknown: false,
    originalRetained: false,
    updatedAt: 1,
    ...over,
  }
}

function walk(opening: string, answers: string[]): { state: OnboardStep; replies: string[]; last: ReturnType<typeof nextOnboard> | null } {
  const started = beginOnboard(opening)
  let state = started.state
  const replies = [started.reply]
  let last: ReturnType<typeof nextOnboard> | null = null
  for (const answer of answers) {
    last = nextOnboard(state, answer)
    replies.push(last.reply)
    if (last.state) state = last.state
  }
  return { state, replies, last }
}

describe('asksOnboarding', () => {
  it.each([
    '帮我开通知识库',
    '我想用知识库，帮我开通',
    '开通一下知识库',
    '帮我建个知识库',
    '我们公司主要做合同审核，帮我建个库把这些合同放进去',
    '启用知识库',
  ])('is a request: %s', (text) => {
    expect(asksOnboarding(text)).toBe(true)
  })

  it.each([
    '知识库里有没有违约金的规定',
    '怎么开通会员',
    '帮我开通微信支付',
    '不用开通知识库',
    '先别开通知识库',
    '知识库开通了吗',
    '知识库开通了没有',
    '把第3段删掉',
    '数据库开通权限',
    '对照一下知识库里的标准条款',
    '建个数据库',
    '建个库',
  ])('is not a request: %s', (text) => {
    expect(asksOnboarding(text)).toBe(false)
  })
})

describe('asksUpload', () => {
  it.each([
    '上传文档到知识库',
    '把文件传到知识库',
    '往知识库里加文档',
  ])('is an upload: %s', (text) => {
    expect(asksUpload(text)).toBe(true)
  })

  it('a question about documents already in the library is not an upload', () => {
    expect(asksUpload('知识库里有哪些文档')).toBe(false)
  })
})

describe('onboarding questions', () => {
  it('starts at the purpose when the opening sentence does not say what it is for', () => {
    const started = beginOnboard('帮我开通知识库')
    expect(started.state).toEqual({ step: 'purpose' })
    expect(started.reply).toBe(PURPOSE)
  })

  it('skips the purpose when the opening sentence already states it', () => {
    const started = beginOnboard('我们公司主要做合同审核，帮我建个库把这些合同放进去')
    expect(started.state).toEqual({ step: 'types', purpose: '合同审核' })
    expect(started.reply).toBe(`好的，我来帮你开通知识库。按你说的，用来「合同审核」。\n${TYPES}`)
    const types = nextOnboard(started.state, '合同')
    expect(types.state).toEqual({ step: 'local', purpose: '合同审核', types: '合同' })
    expect(types.reply).toBe(LOCAL)
  })

  it('asks the three questions, then runs with localOnly false for 否', () => {
    const done = walk('帮我开通知识库', ['合同审核', '合同', '否'])
    expect(done.replies[1]).toBe(TYPES)
    expect(done.replies[2]).toBe(LOCAL)
    expect(done.last).toEqual({ state: null, reply: '', run: { localOnly: false } })
  })

  it.each(['是', '对', '必须', '只在本机', '只放本地', '不能上传'])('yes word %s stays on this computer', (word) => {
    const done = walk('帮我开通知识库', ['合同审核', '合同', word])
    expect(done.last?.run).toEqual({ localOnly: true })
    expect(done.last?.cancelled).toBeUndefined()
  })

  it.each(['否', '不是', '不用', '可以', '没关系', '可以放到服务器', '都行', '否。'])('no word %s may go to the server', (word) => {
    const done = walk('帮我开通知识库', ['合同审核', '合同', word])
    expect(done.last?.run).toEqual({ localOnly: false })
  })

  // #2075 B1: the question's own words, negated server phrases and «是，» + echo all keep the material local.
  it.each([
    '不能放服务器', '不能放到服务器', '不要放服务器', '不可以放服务器', '不放服务器', '别放服务器',
    '资料不能放在服务器上', '要保密不能放服务器', '是，不能放到服务器', '必须只留在这台电脑上、不能放到服务器',
    '只留在这台电脑上', '只留在本机',
  ])('「%s」 stays on this computer', (word) => {
    const done = walk('帮我开通知识库', ['合同审核', '合同', word])
    expect(done.last?.run).toEqual({ localOnly: true })
  })

  // A question back is never consent: asked again.
  it.each(['放服务器安全吗？', '放到服务器会不会泄露', '放服务器'])('「%s」 is asked again, never sent', (word) => {
    const done = walk('帮我开通知识库', ['合同审核', '合同', word])
    expect(done.last?.run).toBeUndefined()
    expect(done.last?.reply).toBe(AGAIN)
  })

  // #2075 B2: refusals do not start it, and bare refusals at the first two questions stop it.
  it.each(['不需要开通知识库', '没必要开通知识库', '无需开通知识库'])('「%s」 does not start onboarding', (text) => {
    expect(asksOnboarding(text)).toBe(false)
  })

  it.each(['不用', '不要', '不需要'])('a bare 「%s」 at the first question stops it', (word) => {
    const done = walk('帮我开通知识库', [word])
    expect(done.last).toEqual({ state: null, reply: CANCEL, cancelled: true })
  })

  it('a purpose that contains 取消 is a purpose, not a cancel', () => {
    const done = walk('帮我开通知识库', ['合同取消条款审核'])
    expect(done.last?.reply).toBe(TYPES)
    expect(done.last?.state).toMatchObject({ step: 'types', purpose: '合同取消条款审核' })
  })

  it('asks once more when the local answer is unclear, then stops', () => {
    const once = walk('帮我开通知识库', ['合同审核', '合同', '嗯'])
    expect(once.last?.run).toBeUndefined()
    expect(once.last?.reply).toBe(AGAIN)
    expect(once.last?.state).toEqual({ step: 'local', purpose: '合同审核', types: '合同', askedLocalAgain: true })
    const twice = nextOnboard(once.last!.state!, '看看')
    expect(twice).toEqual({ state: null, reply: UNCLEAR, cancelled: true })
  })

  it.each([
    ['算了', []],
    ['取消', ['合同']],
    ['不开通了', ['合同', 'pdf']],
    ['先不用', ['合同', 'pdf']],
  ] as const)('cancel word %s stops', (word, earlier) => {
    const done = walk('帮我开通知识库', [...earlier, word])
    expect(done.last).toEqual({ state: null, reply: CANCEL, cancelled: true })
  })
})

describe('preflightReply', () => {
  it('lets onboarding start when the computer is activated and not yet bound', () => {
    expect(preflightReply(ready())).toBeNull()
    expect(preflightReply(ready({ reachable: null }))).toBeNull()
  })

  it('says the library is already connected, ahead of a missing activation', () => {
    expect(preflightReply(ready({ alreadyBound: true, activated: false }))).toBe(BOUND)
  })

  it('says the activation is invalid', () => {
    expect(preflightReply(ready({ invalidActivation: true, activated: false }))).toBe('这台电脑上的激活信息无效，开通不了，请联系提供激活的人重新激活。')
  })

  it('says the computer is not activated', () => {
    expect(preflightReply(ready({ activated: false }))).toBe('这台电脑还没有激活知识库服务，暂时不能开通，请先完成激活。')
  })

  it('says the service cannot be reached', () => {
    expect(preflightReply(ready({ reachable: false }))).toBe('暂时连不上知识库服务，这次没有开通，稍后再试。')
  })
})

describe('resultReply', () => {
  const created = '已在知识库服务器上为你建好默认知识库，之后上传的文档都存放在服务器上。' + FORMAT
  const existing = '你在知识库服务器上的默认知识库之前已经建好了，没有再建新的；上传的文档存放在服务器上。' + FORMAT

  it('describes a new library, and names office formats the user asked for', () => {
    const ok: KnowledgeOnboardResult = { ok: true, outcome: 'created', knowledgeBaseId: 'kb', configMismatch: false, activeNow: true }
    expect(resultReply(ok)).toBe(created)
    expect(resultReply(ok, '合同')).toBe(created)
    for (const types of ['pdf', 'Word', 'doc', 'docx', 'ppt', 'excel', '表格', '扫描件']) {
      expect(resultReply(ok, types), types).toBe(created + OFFICE)
    }
    expect(resultReply(ok, 'document')).toBe(created)
  })

  it('describes a library that was already there', () => {
    expect(resultReply({ ok: true, outcome: 'created', knowledgeBaseId: 'kb', configMismatch: false, activeNow: false }))
      .toContain('还没接上这个知识库')
    const ok: KnowledgeOnboardResult = { ok: true, outcome: 'existing', knowledgeBaseId: 'kb', configMismatch: false, activeNow: true }
    expect(resultReply(ok, 'pdf')).toBe(existing)
  })

  it.each([
    ['local_only_unavailable', '目前只提供放在服务器上的知识库，只在本机的版本还没有，这次没有开通。'],
    ['already_bound', BOUND],
    ['not_activated', '这台电脑还没有激活知识库服务，暂时不能开通，请先完成激活。'],
    ['activation_invalid', '这台电脑上的激活信息无效，开通不了，请联系提供激活的人重新激活。'],
    ['in_progress', '另一次开通还在进行，请稍等一会儿再说一次“帮我开通知识库”。'],
    ['not_offered', '知识库服务暂不提供开通，这次没有开通。'],
    ['unreachable', '没连上知识库服务，这次可能没有开通成功。服务器不会留下半个库，稍后再说一次“帮我开通知识库”即可，不会重复建库。'],
    ['server_error', '开通没有成功（服务出错），没有留下半个库，稍后再试。'],
    ['unavailable', '开通没有成功（服务出错），没有留下半个库，稍后再试。'],
    ['not_saved', '知识库已在服务器上建好，但这台电脑没能保存设置，请再说一次“帮我开通知识库”。'],
  ] as const)('code %s', (code, text) => {
    expect(resultReply({ ok: false, code })).toBe(text)
  })
})

describe('uploadLine', () => {
  it('names each state', () => {
    expect(uploadLine(up({ state: 'refused', reason: 'unsupported_format' }))).toBe('《制度.md》没有上传：这种格式暂不支持（只支持 .md、.txt）')
    expect(uploadLine(up({ state: 'refused', reason: 'too_large' }))).toBe('《制度.md》没有上传：超过 10 MB')
    expect(uploadLine(up({ state: 'refused', reason: 'not_a_file' }))).toBe('《制度.md》没有上传：不是文件')
    expect(uploadLine(up({ state: 'refused', reason: 'not_connected' }))).toBe('《制度.md》没有上传：还没有连上知识库')
    expect(uploadLine(up({ state: 'refused', reason: 'busy' }))).toBe('《制度.md》没有上传：服务器拒绝了（busy）')
    expect(uploadLine(up({ state: 'sending' }))).toBe('《制度.md》正在上传')
    expect(uploadLine(up({ state: 'saved' }))).toBe('《制度.md》已保存，等待处理')
    expect(uploadLine(up({ state: 'processing' }))).toBe('《制度.md》处理中')
    expect(uploadLine(up({ state: 'indexed' }))).toBe('《制度.md》已可检索')
    expect(uploadLine(up({ state: 'indexed', enrichment: 'running' }))).toBe('《制度.md》已可检索，摘要还在生成')
    expect(uploadLine(up({ state: 'failed', reason: 'parse' }))).toBe('《制度.md》处理失败（parse）')
    expect(uploadLine(up({ state: 'cancelled', originalRetained: true }))).toBe('《制度.md》已取消。原件仍保存在服务器上；取消前已发生的处理照实计费')
    expect(uploadLine(up({ state: 'cancelled' }))).toBe('《制度.md》已取消。取消前已发生的处理照实计费')
    expect(uploadLine(up({ state: 'unknown', reason: 'unreachable' }))).toBe('《制度.md》上传后没有收到可用的答复，不确定是否已上传成功')
    expect(uploadLine(up({ state: 'unknown', reason: 'other' }))).toBe('《制度.md》状态不明')
  })

  it('adds a cost suffix only when the cost is final, and zero only then', () => {
    const indexed = up({ state: 'indexed' })
    expect(uploadLine({ ...indexed, costFinal: false, costZero: true })).toBe('《制度.md》已可检索')
    expect(uploadLine({ ...indexed, costFinal: false, costUnknown: true })).toBe('《制度.md》已可检索')
    expect(uploadLine({ ...indexed, costFinal: true, costZero: true, costUnknown: true })).toBe('《制度.md》已可检索，没有产生费用')
    expect(uploadLine({ ...indexed, costFinal: true, costZero: false, costUnknown: true })).toBe('《制度.md》已可检索，费用未知')
    expect(uploadLine({ ...indexed, costFinal: true, costZero: false, costUnknown: false })).toBe('《制度.md》已可检索，已产生费用（以服务器记录为准）')
  })
})
