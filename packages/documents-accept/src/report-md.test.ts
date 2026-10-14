import { describe, expect, it } from 'vitest'
import { type Ran, type Report, buildReport } from './report'
import { renderMarkdown } from './report-md'
import { type ReviewItem, applyReview } from './review'
import { run } from './run'
import { loadAsk } from './samples'
import { HOLIDAYS, NOTICE, type Odd, capabilities, daemon } from './test/daemon'
import { anchor, answer, failed, fakeClient, id, refuse } from './test/fake'

const RAN: Ran = { commit: 'abc1234', machine: { model: 'Mac16,10', os: 'macOS 15.2' }, at: new Date('2026-10-14T08:00:00Z'), secrets: [] }
const here = (block: string) => anchor(1, block, 0, 4, { content_sha256: `sha256:${NOTICE.meta.sha256_stored}` })
const SAID: Record<string, object> = {
  lift_in_service: { status: 'present', value: '1号电梯照常运行\n| 是', anchors: [here('p1/b9')] },
  contact_party: { status: 'present', value: '服务中心', anchors: [here('p1/b1')] },
  work_order: { status: 'present', value: 'WO-000731', anchors: [here('p1/b1')] },
  store_water_in_advance: { status: 'present', value: '不用储水', anchors: [here('p1/b1')] },
  contact_phone: { status: 'present', value: '12345', anchors: [here('p1/b1')] },
}
const values = loadAsk(NOTICE).map((f) => ({ key: f.key, ...(SAID[f.key] ?? { status: 'missing', missing_reason: 'not_in_document' }) }))
const both = [NOTICE, HOLIDAYS].map((sample) => ({ sample, ask: loadAsk(sample) }))
/** The report of a run on the stand-in; the job of the sample to be cancelled runs until then. */
const report = async (odd: Odd = { values }, samples = both.slice(0, 1), options: { cancelOn?: string; replayOn?: string } = {}): Promise<Report> =>
  buildReport(await run(fakeClient(daemon({ slow: options.cancelOn, ...odd })).client, samples, { sleep: async () => {}, ...options }), [NOTICE, HOLIDAYS], RAN)
const on = (more: Partial<ReviewItem>): ReviewItem => ({ sample: NOTICE.id, extraction_id: id('ext'), verdict: 'correct', ...more })
const reviewed = (r: Report, items: ReviewItem[]) => applyReview(r, { reviewer: 'david', items })
/** The lines of one section, from its heading to the next of its level. */
const section = (md: string, heading: string): string[] => {
  const lines = md.split('\n')
  const start = lines.indexOf(heading)
  expect(start, heading).toBeGreaterThanOrEqual(0)
  const end = lines.findIndex((l, i) => i > start && l.startsWith('## '))
  return lines.slice(start + 1, end === -1 ? undefined : end).filter((l) => l !== '')
}
const S = NOTICE.id

describe('renderMarkdown', () => {
  it('opens with what ran, on what', async () => {
    const md = renderMarkdown(await report())
    expect(md.split('\n').slice(0, 9)).toEqual([
      '# Documenting S01 验收报告',
      '',
      '- 生成时间：2026-10-14T08:00:00.000Z',
      '- 仓库 commit：`abc1234`；机器：Mac16,10，macOS 15.2',
      '- Documents OS：0.1.0；引擎：apple-pdfkit 15.2.0（parse，ready）、apple-pdfkit 15.2.0（render，ready）',
      '- 模型：`qwen2.5:7b`；extractor：2',
      `- 跑了 1 份样本：${S}`,
      '- 幂等检查：`ACC-IDEM-1`、`ACC-IDEM-2` 没有指定样本；`ACC-IDEM-3` 没有指定样本。',
      '',
    ])
    expect(md).not.toContain('没有读到抽取结果的样本')
    expect(md).not.toContain('不止一个模型')
    expect(md).not.toContain('没有跑的样本')
  })

  it('says where the idempotency checks ran and what they met, and when the sample named for them was not run', async () => {
    const idem = (md: string) => md.split('\n').find((l) => l.startsWith('- 幂等检查：'))
    expect(idem(renderMarkdown(await report({ values }, both.slice(0, 1), { replayOn: S, cancelOn: S })))).toBe(`- 幂等检查：\`ACC-IDEM-1\`、\`ACC-IDEM-2\` 在 ${S} 上，重放时 job 已结束，原样返回；\`ACC-IDEM-3\` 在 ${S} 上，取消时 job 没有在运行。`)
    const elsewhere = await report({ values }, both.slice(0, 1), { replayOn: HOLIDAYS.id, cancelOn: HOLIDAYS.id })
    expect(idem(renderMarkdown(elsewhere))).toBe(`- 幂等检查：\`ACC-IDEM-1\`、\`ACC-IDEM-2\` 指定在 ${HOLIDAYS.id} 上，这次没有跑这份样本；\`ACC-IDEM-3\` 指定在 ${HOLIDAYS.id} 上，这次没有跑这份样本。`)
    const one = await report({ values }, both.slice(0, 1), { replayOn: S, cancelOn: S })
    const met: Report = { ...one, samples: one.samples.map((s) => ({ ...s, requeued: true, interrupted: true })) }
    expect(idem(renderMarkdown(met))).toBe(`- 幂等检查：\`ACC-IDEM-1\`、\`ACC-IDEM-2\` 在 ${S} 上，重放时 job 已失败，被重新排队；\`ACC-IDEM-3\` 在 ${S} 上，取消时 job 正在运行。`)
    // The job had succeeded when the cancel came: there was nothing to cancel.
    const fast = await report({ values, slow: undefined }, both.slice(0, 1), { cancelOn: S })
    expect(idem(renderMarkdown(fast))).toBe(`- 幂等检查：\`ACC-IDEM-1\`、\`ACC-IDEM-2\` 没有指定样本；\`ACC-IDEM-3\` 在 ${S} 上，取消到达时 job 已经成功，未执行。`)
    const refused = await report({ values, retry: () => refuse(409, 'job_not_retryable') }, both.slice(0, 1), { cancelOn: S })
    expect(idem(renderMarkdown(refused))).toBe(`- 幂等检查：\`ACC-IDEM-1\`、\`ACC-IDEM-2\` 没有指定样本；\`ACC-IDEM-3\` 在 ${S} 上，取消时 job 没有在运行。`)
    const unread = await report({ values, imported: failed('unsupported_format', { job_id: id('job', 9), kind: 'import' }) }, both.slice(0, 1), { cancelOn: S })
    expect(idem(renderMarkdown(unread))).toBe(`- 幂等检查：\`ACC-IDEM-1\`、\`ACC-IDEM-2\` 没有指定样本；\`ACC-IDEM-3\` 在 ${S} 上，没有取消到 job。`)
    const failedRun = await report({ ends: failed('engine_unavailable') }, both.slice(0, 1), { replayOn: S })
    expect(idem(renderMarkdown(failedRun))).toBe(`- 幂等检查：\`ACC-IDEM-1\`、\`ACC-IDEM-2\` 在 ${S} 上，没有做成；\`ACC-IDEM-3\` 没有指定样本。`)
  })

  it('says of a sample whose extraction was not read that its fields count as unanswered, and of a find that was not read that its anchors count as not reached', async () => {
    const md = renderMarkdown(await report({ ends: failed('engine_unavailable'), find: () => refuse(503, 'engine_unavailable') }))
    expect(md).toContain(`- **没有读到抽取结果的样本：${S}。它们的字段都按“没有回答”计入下面的数字。**`)
    expect(md).toContain('| 字段正确率 | 0 / 17（0.0%） | 0 / 3（0.0%） |')
    // Its critical errors are no errors of a model: they say so.
    const errors = section(md, '## 严重错误')
    expect(errors).toContain(`- ${S} · \`shutoff_date\`：漏掉（没有读到抽取结果）`)
    expect(errors).toContain(`- ${S} · \`contact_phone\`：未回答（没有读到抽取结果）`)
    expect(section(renderMarkdown(await report()), '## 严重错误').join('\n')).not.toContain('没有读到抽取结果')
    expect(md).toContain('- `find` 的可达率：0 / 58（0.0%）。其中 1 份样本的 `find` 没有读到，它们的金标锚点按未达计。')
  })

  it('lets nothing the daemon or a document says start a heading, close a cell, or open a code span or a tag', async () => {
    const caps = { ...capabilities(), os_version: '0.1.0\n# 验收结论：通过\n', engines: [{ id: 'pdf|kit', kind: 'parse', formats: [], state: 'ready', version: '15.2.0\n## 没有通过的检查\n无。' }, { id: 'apple-pdfkit', kind: 'render', formats: [], state: 'ready', version: null }] }
    const tricky = values.map((v) => (v.key === 'lift_in_service' ? { ...v, value: '1号电梯照常运行 `x` <b>|\n# 标题' } : v))
    const one = await report({ values: tricky, capabilities: () => answer(caps) })
    const md = renderMarkdown({ ...one, environment: { ...one.environment, model_ids: ['qwen`2.5\n# x'], extractor_versions: ['2\n## y'] } })
    const lines = md.split('\n')
    expect(lines.filter((l) => l.startsWith('# '))).toEqual(['# Documenting S01 验收报告'])
    expect(lines.filter((l) => l.startsWith('## '))).toEqual(['## 检查项（追溯链）', '## 没有通过的检查', '## 汇总（验收设计 §4.8）', '## 覆盖点', '## 严重错误', '## 待复核'])
    expect(lines[4]).toBe('- Documents OS：0.1.0 # 验收结论：通过；引擎：pdf\\|kit 15.2.0 ## 没有通过的检查 无。（parse，ready）、apple-pdfkit ?（render，ready）')
    expect(lines[5]).toBe("- 模型：`qwen'2.5 # x`；extractor：2 ## y")
    expect(md).toContain('结果 present “1号电梯照常运行 \\`x\\` \\<b>\\| # 标题”')
    // The id of a sample is the name of a directory: it too is shown as one line of text.
    const odd = 's01-07|x `y`\n# z'
    const renamed = renderMarkdown(reviewed({ ...one, unverified: [odd], samples: one.samples.map((s) => ({ ...s, sample: odd, checks: s.checks.map((c) => ({ ...c, sample: odd, outcome: 'failed' as const })) })) }, [{ ...on({ must_not_assert: 1, verdict: 'violated' }), sample: odd }]))
    expect(renamed.split('\n').filter((l) => l.startsWith('#') && !l.startsWith('## ') && l !== '# Documenting S01 验收报告')).toEqual([])
    expect(renamed.split('\n').filter((l) => l.includes('# z') && !l.includes('s01-07\\|x \\`y\\` # z'))).toEqual([])
    const notRead = renderMarkdown({ ...one, samples: one.samples.map((s) => ({ ...s, sample: odd, extraction_id: undefined })) })
    expect(notRead).toContain('- **没有读到抽取结果的样本：s01-07\\|x \\`y\\` # z。')
    expect(notRead.split('\n').filter((l) => l.startsWith('# '))).toEqual(['# Documenting S01 验收报告'])
    expect(renderMarkdown({ ...one, environment: { ...one.environment, commit: 'abc`1\n# x' } }).split('\n')[3]).toBe("- 仓库 commit：`abc'1 # x`；机器：Mac16,10，macOS 15.2")
  })

  it('says so when the numbers are of more than one model, when samples were left out, and when nothing ran', async () => {
    const one = await report({}, both)
    const mixed: Report = { ...one, environment: { ...one.environment, model_ids: ['qwen2.5:7b', 'qwen2.5:14b'] } }
    expect(renderMarkdown(mixed)).toContain('- 模型：`qwen2.5:7b`、`qwen2.5:14b`；extractor：2\n- **这次运行里出现了不止一个模型或 extractor 版本，下面的数字不能当成同一种配置的结果。**')
    expect(renderMarkdown({ ...one, environment: { ...one.environment, extractor_versions: ['2', '3'] } })).toContain('不止一个模型或 extractor 版本')
    const draft = { sample: { ...NOTICE, gold: { ...NOTICE.gold, verified_by: null as never } }, ask: both[0]!.ask }
    expect(renderMarkdown(await report({}, [draft, both[1]!]))).toContain(`- 金标未经核对、没有跑的样本：${S}\n- 跑了 1 份样本：${HOLIDAYS.id}`)
    const unmet = renderMarkdown(await report({ capabilities: () => answer(capabilities(false)) }))
    expect(unmet).toContain('- **环境不满足，没有跑任何样本：** extract is not available (engine_unavailable)')
    expect(unmet).toContain('- 模型：没有抽取结果；extractor：—\n')
    expect(unmet).toContain('- 跑了 0 份样本：无')
    expect(section(unmet, '## 没有通过的检查')).toEqual(['- `ACC-CAP`：环境不满足。extract is not available (engine_unavailable)'])
    const silent = renderMarkdown(await report({ capabilities: () => refuse(404, 'not_found') }))
    expect(silent).toContain('- **失败，没有跑任何样本：** 404 not_found: it is not_found')
    expect(silent).toContain('- Documents OS：未知；引擎：未知')
  })

  it('has the trace chain as a table, a row for every check, with its conclusion in words', async () => {
    const rows = section(renderMarkdown(await report({ values, text: () => refuse(422, 'parse_failed') }, both.slice(0, 1), { replayOn: S })), '## 检查项（追溯链）')
    expect(rows).toHaveLength(12)
    expect(rows[0]).toBe('| 场景 / 能力 | 契约 | 检查 | 负责人 / 阶段 | 结论 | 各样本的结果 |')
    expect(rows[2]).toBe('| A2 A3 A5 | ADR-DOC-02 §8 | `ACC-CAP` | David / DOC-1 | 通过 | 通过 1 |')
    expect(rows[5]).toBe('| A2 A3 | ADR-DOC-02 §3.1 | `ACC-TEXT` | David / DOC-1 | 失败 | 失败 1 |')
    expect(rows[9]).toBe('| README §14 | ADR-DOC-02 §5.7 (1) | `ACC-IDEM-1` | David / DOC-1 | 通过 | 通过 1 |')
    expect(rows[11]).toBe('| README §14 | ADR-DOC-02 §5.7 (3), §7 | `ACC-IDEM-3` | David / DOC-1 | 未运行 | — |')
  })

  it('lists every check that did not pass, with what was wrong, and not the ones that passed or had nothing to do', async () => {
    const md = renderMarkdown(await report({ ends: failed('engine_unavailable') }, both.slice(0, 1), { replayOn: S }))
    expect(section(md, '## 没有通过的检查')).toEqual([
      `- \`ACC-EXTRACT\` · ${S}：失败。the extraction job is failed: engine_unavailable`,
      `- \`ACC-IDEM-1\` · ${S}：未运行（前置检查未通过）。no extraction was read to replay`,
      `- \`ACC-IDEM-2\` · ${S}：未运行（前置检查未通过）。no extraction was read to replay`,
    ])
    expect(md).toContain('| `ACC-IDEM-1` | David / DOC-1 | 未运行（前置检查未通过） | 未运行（前置检查未通过） 1 |')
    const fine = await report({}, both)
    expect(section(renderMarkdown(fine), '## 没有通过的检查')).toEqual(['无。'])
    const skipped: Report = { ...fine, samples: fine.samples.map((s) => ({ ...s, checks: [...s.checks, { id: 'ACC-IDEM-3', sample: s.sample, outcome: 'skipped' as const, problems: [], ms: 0 }] })) }
    expect(section(renderMarkdown(skipped), '## 没有通过的检查')).toEqual(['无。'])
  })

  it('gives the numbers of §4.8, the answer fields apart from the context ones', async () => {
    const rows = section(renderMarkdown(await report()), '## 汇总（验收设计 §4.8）')
    expect(rows).toEqual([
      '| 指标 | 必答字段 | 背景字段 |',
      '|---|---|---|',
      '| 字段正确率 | 0 / 17（0.0%） | 1 / 3（33.3%） |',
      '| 待复核的字段 | 2 | 0 |',
      '| `missing` 判对率 | 0 / 1（0.0%） | 0 / 0 |',
      '| `conflict` 判对率 | 0 / 2（0.0%） | 0 / 0 |',
      '| 锚点命中率 | 4 / 6（66.7%），其中 `find` 定位不到 0 条 | 1 / 1（100.0%），其中 `find` 定位不到 0 条 |',
      '| 严重错误数 | 12 | 2 |',
      '| 待复核（严重） | 1 | 0 |',
      '- `must_not_assert`：共 11 条，违规 0 条，未评审 11 条。',
      '- `find` 的可达率：58 / 58（100.0%）。',
      '- 还没有人工复核；字段正确率在复核之前是下限。',
    ])
    // The reach of find is its own number: the stand-in finds the quotes of page 1 only.
    const two = await report({}, both)
    expect(two.find.reachable).toBeLessThan(two.find.anchors)
    expect(renderMarkdown(two)).toContain(`- \`find\` 的可达率：${two.find.reachable} / ${two.find.anchors}（${((100 * two.find.reachable) / two.find.anchors).toFixed(1)}%）。`)
  })

  it('sets the numbers after review beside the mechanical ones', async () => {
    const after = reviewed(await report(), [on({ field: 'lift_in_service' }), on({ field: 'lift_in_service', anchor: 1, verdict: 'covered' }), on({ must_not_assert: 6, verdict: 'violated', note: '把两处的\n时刻拼在一起' }), on({ must_not_assert: 1, verdict: 'not_violated' }), on({ field: 'nope' })])
    const rows = section(renderMarkdown(after), '## 汇总（验收设计 §4.8）')
    expect(rows[0]).toBe('| 指标 | 必答字段 | 背景字段 | 必答字段（复核后） | 背景字段（复核后） |')
    expect(rows[2]).toBe('| 字段正确率 | 0 / 17（0.0%） | 1 / 3（33.3%） | 1 / 17（5.9%） | 1 / 3（33.3%） |')
    expect(rows[3]).toBe('| 待复核的字段 | 2 | 0 | 1 | 0 |')
    expect(rows[6]).toBe('| 锚点命中率 | 4 / 6（66.7%），其中 `find` 定位不到 0 条 | 1 / 1（100.0%），其中 `find` 定位不到 0 条 | 5 / 6（83.3%），其中 `find` 定位不到 0 条 | 1 / 1（100.0%），其中 `find` 定位不到 0 条 |')
    expect(rows[8]).toBe('| 待复核（严重） | 1 | 0 | 0 | 0 |')
    expect(rows.slice(9)).toEqual(['- `must_not_assert`：共 11 条，违规 1 条，未评审 9 条。', '- `find` 的可达率：58 / 58（100.0%）。', '- 人工复核：david，采用 4 条，未采用 1 条。'])
  })

  it('shows every coverage point with the fields it stands on, and how it came out, before and after review', async () => {
    const before = await report()
    const rows = section(renderMarkdown(before), '## 覆盖点')
    expect(rows.slice(0, 2)).toEqual(['| 样本 | 覆盖点 | 字段 | 结论 |', '|---|---|---|---|'])
    expect(rows.slice(2)).toHaveLength(Object.keys(NOTICE.meta.covers).length)
    expect(rows).toContain(`| ${S} | C9 | \`contact_phone\` | 失败 |`)
    expect(rows).toContain(`| ${S} | C7 | \`affected_units\`、\`drinking_water_limit\` | 失败 |`)
    const after = section(renderMarkdown(reviewed(before, [on({ field: 'contact_phone' }), on({ field: 'affected_units' })])), '## 覆盖点')
    expect(after[0]).toBe('| 样本 | 覆盖点 | 字段 | 结论 | 复核后 |')
    expect(after).toContain(`| ${S} | C9 | \`contact_phone\` | 失败 | 通过 |`)
    expect(after).toContain(`| ${S} | C7 | \`affected_units\`、\`drinking_water_limit\` | 失败 | 失败 |`)
    // A point that waits for a person says so.
    const waiting: Report = { ...before, samples: before.samples.map((s) => ({ ...s, coverage: { ...s.coverage, C9: 'review' as const, C6: 'passed' as const } })) }
    const shown = section(renderMarkdown(waiting), '## 覆盖点')
    expect(shown).toContain(`| ${S} | C9 | \`contact_phone\` | 待复核 |`)
    expect(shown).toContain(`| ${S} | C6 | \`shutoff_registration_not_needed\` | 通过 |`)
  })

  it('lists the critical errors by what went wrong, the context fields apart, and what a person overturned as overturned', async () => {
    const before = await report()
    const errors = section(renderMarkdown(before), '## 严重错误')
    expect(errors[0]).toBe('必答字段：')
    expect(errors.slice(1, errors.indexOf('背景字段（不计入）：'))).toHaveLength(12)
    expect(errors).toContain(`- ${S} · \`shutoff_date\`：漏掉`)
    expect(errors).toContain(`- ${S} · \`contact_phone\`：编造`)
    // A major field that is wrong is no critical error, and one that waits for a person is not yet one.
    expect(errors.join('\n')).not.toMatch(/store_water_in_advance|lift_in_service|contact_party/)
    expect(errors.slice(errors.indexOf('背景字段（不计入）：') + 1, errors.indexOf('`must_not_assert` 违规：'))).toEqual([`- ${S} · \`notice_reference\`：漏掉`, `- ${S} · \`notice_date\`：漏掉`])
    expect(errors.slice(-2)).toEqual(['`must_not_assert` 违规：', '无。'])
    const after = section(renderMarkdown(reviewed(before, [on({ field: 'lift_in_service', verdict: 'mismatch' }), on({ field: 'contact_phone' }), on({ must_not_assert: 1, verdict: 'not_violated' }), on({ must_not_assert: 6, verdict: 'violated', note: '把两处的\n时刻拼在一起' })])), '## 严重错误')
    expect(after[0]).toBe('按复核后的结论列出。')
    expect(after).toContain(`- ${S} · \`lift_in_service\`：值不符（david 复核；机械结论：待复核）`)
    expect(after.join('\n')).not.toContain('contact_phone')
    expect(after.slice(-2)).toEqual(['`must_not_assert` 违规：', `- ${S} · 第 6 条：${NOTICE.gold.must_not_assert[5]!.proposition}（把两处的 时刻拼在一起）`])
  })

  it('lists what waits for a person: the fields with both values, critical ones first; the anchors not covered; the propositions not judged', async () => {
    const before = await report()
    const waiting = section(renderMarkdown(before), '## 待复核')
    expect(waiting.slice(0, 3)).toEqual([
      '字段（严重的在前）：',
      `- **严重** ${S} · \`lift_in_service\`：金标 present “1号电梯照常运行”，结果 present “1号电梯照常运行 \\| 是”`,
      `- ${S} · \`contact_party\`：金标 present “物业服务中心”，结果 present “服务中心”`,
    ])
    const anchors = waiting.slice(waiting.indexOf('未覆盖的金标锚点：') + 1, waiting.indexOf('`must_not_assert`（未评审的）：'))
    expect(anchors).toEqual([`- ${S} · \`lift_in_service\` · 锚点 1 · 第 1 页 “1号电梯照常运行。”`, `- ${S} · \`lift_in_service\` · 锚点 2 · 第 1 页 “Lift 1 remains in service.”`])
    const propositions = waiting.slice(waiting.indexOf('`must_not_assert`（未评审的）：') + 1)
    expect(propositions).toHaveLength(11)
    expect(propositions[0]).toBe(`- ${S} · 第 1 条：${NOTICE.gold.must_not_assert[0]!.proposition}`)
    expect(propositions[5]).toBe(`- ${S} · 第 6 条：停水持续的具体时长（除非：${NOTICE.gold.must_not_assert[5]!.unless}）`)
    // What a person has decided no longer waits; an anchor a person found uncovered now does.
    const after = section(renderMarkdown(reviewed(before, [on({ field: 'lift_in_service' }), on({ field: 'lift_in_service', anchor: 1, verdict: 'covered' }), on({ field: 'work_order', anchor: 1, verdict: 'uncovered' }), on({ must_not_assert: 1, verdict: 'not_violated' })])), '## 待复核')
    expect(after.slice(0, 3)).toEqual(['字段（严重的在前）：', `- ${S} · \`contact_party\`：金标 present “物业服务中心”，结果 present “服务中心”`, '未覆盖的金标锚点：'])
    // The number is the one a review names the anchor by: it stays when another of the field is settled.
    expect(after.slice(3, 5)).toEqual([`- ${S} · \`work_order\` · 锚点 1 · 第 1 页 “工单号 Work Order: WO-000731”`, `- ${S} · \`lift_in_service\` · 锚点 2 · 第 1 页 “Lift 1 remains in service.”`])
    expect(after.slice(after.indexOf('`must_not_assert`（未评审的）：') + 1)).toHaveLength(10)
  })

  it('lists the critical fields that wait before the others, whatever their order in the gold', async () => {
    // In the gold the major field comes first; each answer says a little more than the gold.
    const keys = ['drinking_water_registration_location', 'drinking_water_limit']
    const more = loadAsk(NOTICE).map((f) => {
      const gold = NOTICE.gold.fields.find((g) => g.key === f.key)!
      return keys.includes(f.key) ? { key: f.key, status: 'present', value: `${gold.value}（另）`, anchors: [here('p1/b1')] } : { key: f.key, status: 'missing', missing_reason: 'not_in_document' }
    })
    const waiting = section(renderMarkdown(await report({ values: more })), '## 待复核')
    const fields = waiting.slice(1, waiting.indexOf('未覆盖的金标锚点：'))
    expect(fields.map((l) => keys.find((k) => l.includes(`\`${k}\``)))).toEqual(['drinking_water_limit', 'drinking_water_registration_location'])
    expect(fields[0]).toMatch(/^- \*\*严重\*\* /)
  })

  it('marks the anchors find could not place, and names why the candidates of a conflict wait', async () => {
    const side = (value: string, normalized: string) => ({ value, normalized, anchors: [here('p1/b1')] })
    const conflict = { key: 'shutoff_date', status: 'conflict', candidates: [side('10月12日', '2026-10-12'), side('10月14日', '2026-10-14')] }
    const said = values.map((v) => (v.key === 'shutoff_date' ? conflict : v))
    let asked = 0
    const waiting = section(renderMarkdown(await report({ values: said, find: () => ((asked += 1) > 40 ? answer({ parse_status: 'complete', matches: [], next_cursor: null }) : answer({ parse_status: 'complete', matches: [here('p1/b1')], next_cursor: null })) })), '## 待复核')
    expect(waiting).toContain(`- **严重** ${S} · \`shutoff_date\`：金标 conflict，结果 conflict；候选：incomplete、extra`)
    expect(waiting.some((l) => l.endsWith('（`find` 定位不到）'))).toBe(true)
    expect(waiting.some((l) => l.includes('第 1 页') && !l.endsWith('（`find` 定位不到）'))).toBe(true)
  })

  it('lists the decisions of a review that changed nothing, with why, and has no such section without them', async () => {
    const before = await report()
    expect(renderMarkdown(before)).not.toContain('## 没有采用的复核条目')
    expect(renderMarkdown(reviewed(before, [on({ field: 'lift_in_service' })]))).not.toContain('## 没有采用的复核条目')
    const md = renderMarkdown(reviewed(before, [on({ field: 'nope' }), on({ must_not_assert: 99, verdict: 'violated' }), on({ field: 'lift_in_service', anchor: 9, verdict: 'covered' })]))
    expect(section(md, '## 没有采用的复核条目')).toEqual([
      `- ${S} · nope：the sample has no field nope`,
      `- ${S} · must_not_assert 99：the sample has no must_not_assert 99`,
      `- ${S} · lift_in_service · 锚点 9：the field lift_in_service has no scored anchor 9`,
    ])
  })
})
