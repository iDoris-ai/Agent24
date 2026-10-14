// `report.md` (acceptance README §5): the summary of §4.8, the critical
// errors and what waits for a person, for a person to read. Everything in it
// is in `report.json`; nothing is decided here.

import type { CheckResult } from './checks'
import type { FieldReport, Report, TraceRow } from './report'
import type { Reviewed } from './review'
import type { Verdict } from './score'
import type { Totals } from './summary'

const VERDICT: Record<Verdict, string> = {
  correct: '判对',
  review: '待复核',
  mismatch: '值不符',
  missed: '漏掉',
  fabricated: '编造',
  chose_one: '擅自取舍',
  false_conflict: '误报冲突',
  unanswered: '未回答',
}
const OUTCOME: Record<TraceRow['conclusion'], string> = {
  passed: '通过',
  failed: '失败',
  unmet: '环境不满足',
  skipped: '未执行',
  blocked: '未运行（前置检查未通过）',
  not_run: '未运行',
}

/** Text from elsewhere as one cell of a table or one line of a list: it cannot
 *  start a line, close a cell, open a code span or a tag of its own. */
export const line = (text: string): string => text.replace(/\s+/g, ' ').replace(/[\\|`<]/g, '\\$&').trim()
/** Text from elsewhere as code. */
const code = (text: string): string => `\`${text.replace(/\s+/g, ' ').replace(/`/g, "'").trim()}\``
const STATE = { passed: '通过', review: '待复核', failed: '失败' }
/** Rounded, but never to 100.0% short of the whole, nor to 0.0% above nothing. */
export const ratio = (part: number, whole: number): string => {
  const share = Math.round((1000 * part) / whole) / 10
  return whole === 0 ? '0 / 0' : `${part} / ${whole}（${(part > 0 && part < whole ? Math.min(Math.max(share, 0.1), 99.9) : share).toFixed(1)}%）`
}
const table = (head: string[], rows: string[][]): string[] => [`| ${head.join(' | ')} |`, `|${head.map(() => '---').join('|')}|`, ...rows.map((r) => `| ${r.join(' | ')} |`), '']
const list = (items: string[]): string[] => (items.length === 0 ? ['无。', ''] : [...items.map((i) => `- ${i}`), ''])

/** The rows of §4.8 for one kind of field. */
function numbers(t: Totals): string[] {
  return [
    ratio(t.correct, t.fields),
    String(t.review),
    ratio(t.missing.correct, t.missing.fields),
    ratio(t.conflict.correct, t.conflict.fields),
    `${ratio(t.anchors.covered, t.anchors.gold)}，其中 \`find\` 定位不到 ${t.anchors.unlocated} 条`,
    String(t.critical.length),
    String(t.criticalReview.length),
  ]
}
const ROWS = ['字段正确率', '待复核的字段', '`missing` 判对率', '`conflict` 判对率', '锚点命中率', '严重错误数', '待复核（严重）']

function summary(report: Report, review?: Reviewed): string[] {
  const columns: [string, Totals][] = [
    ['必答字段', report.totals.answer],
    ['背景字段', report.totals.context],
    ...(review ? ([['必答字段（复核后）', review.totals.answer], ['背景字段（复核后）', review.totals.context]] as [string, Totals][]) : []),
  ]
  const cells = columns.map(([, t]) => numbers(t))
  const asserted = report.samples.reduce((n, s) => n + s.must_not_assert.length, 0)
  return [
    ...table(['指标', ...columns.map(([name]) => name)], ROWS.map((name, i) => [name, ...cells.map((c) => c[i] ?? '')])),
    `- \`must_not_assert\`：共 ${asserted} 条，违规 ${review?.must_not_assert.violated ?? 0} 条，未评审 ${review?.must_not_assert.unreviewed ?? asserted} 条。`,
    `- \`find\` 的可达率：${ratio(report.find.reachable, report.find.anchors)}。${report.find.unread > 0 ? `其中 ${report.find.unread} 份样本的 \`find\` 没有读到，它们的金标锚点按未达计。` : ''}`,
    // Each quote find did not reach, and why, as report.json has it (§4.4).
    ...report.samples.flatMap((s) => (s.find?.unreached ?? []).map((u) => `  - \`find\` 没有找到：${line(s.sample)} · 第 ${u.page} 页 “${line(u.quote)}”${u.refused ? `（被拒绝：${line(u.refused)}）` : (u.unparsed?.length ?? 0) > 0 ? '（文档有未解析的区域）' : ''}`)),
    ...(review ? [`- 人工复核：${line(review.reviewer)}，采用 ${review.applied} 条，未采用 ${review.unused.length} 条。`] : ['- 还没有人工复核；字段正确率在复核之前是下限。']),
    '',
  ]
}

const failing = (c: CheckResult): boolean => c.outcome !== 'passed' && c.outcome !== 'skipped'

/** The report for a person to read. */
export function renderMarkdown(report: Report & { review?: Reviewed }): string {
  const { environment: env, review } = report
  // `sample` is the id as it is shown: it is the name of a directory, and held to nothing.
  const fields = report.samples.flatMap((s) => s.fields.map((f) => ({ sample: line(s.sample), unread: s.extraction_id === undefined, ...f, verdict: f.review?.verdict ?? f.score.verdict })))
  const answers = fields.filter((f) => f.role === 'answer')
  const wrong = (f: (typeof fields)[number]) => f.severity === 'critical' && f.verdict !== 'correct' && f.verdict !== 'review'
  const named = (f: (typeof fields)[number]) => `${f.sample} · \`${f.key}\`：${VERDICT[f.verdict]}${f.unread ? '（没有读到抽取结果）' : ''}${f.review ? `（${line(f.review.reviewer)} 复核；机械结论：${VERDICT[f.score.verdict]}）` : ''}`
  // What a person found no error, of what the machine had found one.
  const overturned = (role: FieldReport['role']) => fields.filter((f) => f.role === role && f.severity === 'critical' && f.review && !wrong(f) && f.score.verdict !== 'correct' && f.score.verdict !== 'review').map(named)
  const said = (f: (typeof fields)[number]) => `金标 ${f.gold.status}${f.gold.value ? ` “${line(f.gold.value)}”` : ''}，结果 ${f.result?.status ?? '无'}${f.result?.value ? ` “${line(f.result.value)}”` : ''}`
  const waiting = fields.filter((f) => f.verdict === 'review').sort((a, b) => Number(b.severity === 'critical') - Number(a.severity === 'critical'))
  // The number of an anchor is its place among the field's anchors in report.json: what a review names it by (§4.7).
  const uncovered = fields.flatMap((f) => (f.anchors?.anchors ?? []).flatMap((a, i) => ((a.review ? a.review.verdict === 'uncovered' : a.by === undefined) ? [`${f.sample} · \`${f.key}\` · 锚点 ${i + 1} · 第 ${a.gold.page} 页 “${line(a.gold.quote)}”${a.unlocated ? '（`find` 定位不到）' : ''}`] : [])))
  const unread = report.samples.filter((s) => s.extraction_id === undefined).map((s) => line(s.sample))
  const ran = (id: string | undefined, met: (s: Report['samples'][number]) => string): string => {
    const sample = report.samples.find((s) => s.sample === id)
    return id === undefined ? '没有指定样本' : sample ? `在 ${line(id)} 上，${met(sample)}` : `指定在 ${line(id)} 上，这次没有跑这份样本`
  }
  // What the replay met is said only of one that passed: of one that failed, the check says what went wrong.
  const outcome = (s: Report['samples'][number], id: string) => s.checks.find((c) => c.id === id)?.outcome
  const replayed = ran(report.idempotency.replayOn, (s) => (outcome(s, 'ACC-IDEM-1') === 'failed' ? '重放没有通过，见“没有通过的检查”' : s.requeued === undefined ? '没有做成' : s.requeued ? '重放时 job 已失败，被重新排队' : '重放时 job 已结束，原样返回'))
  const skipped = (s: Report['samples'][number]) => s.checks.some((c) => c.id === 'ACC-IDEM-3' && c.outcome === 'skipped')
  const cancelled = ran(report.idempotency.cancelOn, (s) => (outcome(s, 'ACC-IDEM-3') === 'failed' ? '取消的检查没有通过，见“没有通过的检查”' : skipped(s) ? '取消到达时 job 已经成功，未执行' : s.interrupted === undefined ? '没有取消到 job' : s.interrupted ? '取消时 job 正在运行' : '取消时 job 没有在运行'))
  const checks = [report.capabilities, ...report.samples.flatMap((s) => s.checks)]
  return [
    '# Documenting S01 验收报告',
    '',
    `- 生成时间：${report.generated_at}`,
    `- 仓库 commit：${code(env.commit)}；机器：${line(env.machine.model)}，${line(env.machine.os)}`,
    `- Documents OS：${line(env.os_version ?? '未知')}；引擎：${env.engines.map((e) => `${line(e.id)} ${line(e.version ?? '?')}（${line(e.kind)}，${line(e.state)}）`).join('、') || '未知'}`,
    `- 模型：${env.model_ids.map(code).join('、') || '没有抽取结果'}；extractor：${env.extractor_versions.map(line).join('、') || '—'}`,
    ...(env.model_ids.length > 1 || env.extractor_versions.length > 1 ? ['- **这次运行里出现了不止一个模型或 extractor 版本，下面的数字不能当成同一种配置的结果。**'] : []),
    ...(report.capabilities.outcome === 'passed' ? [] : [`- **${OUTCOME[report.capabilities.outcome]}，没有跑任何样本：** ${report.capabilities.problems.map(line).join('；')}`]),
    ...(report.unverified.length > 0 ? [`- 金标未经核对、没有跑的样本：${report.unverified.map(line).join('、')}`] : []),
    `- 跑了 ${report.samples.length} 份样本：${report.samples.map((s) => line(s.sample)).join('、') || '无'}`,
    ...(unread.length > 0 ? [`- **没有读到抽取结果的样本：${unread.join('、')}。它们的字段都按“没有回答”计入下面的数字。**`] : []),
    `- 幂等检查：\`ACC-IDEM-1\`、\`ACC-IDEM-2\` ${replayed}；\`ACC-IDEM-3\` ${cancelled}。`,
    '',
    '## 检查项（追溯链）',
    '',
    ...table(['场景 / 能力', '契约', '检查', '负责人 / 阶段', '结论', '各样本的结果'], report.trace.map((t) => [t.scenario, t.contract, `\`${t.check}\``, `${t.owner} / ${t.phase}`, OUTCOME[t.conclusion], Object.entries(t.results).map(([o, n]) => `${OUTCOME[o as CheckResult['outcome']]} ${n}`).join('，') || '—'])),
    '## 没有通过的检查',
    '',
    ...list(checks.filter(failing).map((c) => `\`${c.id}\`${c.sample ? ` · ${line(c.sample)}` : ''}：${OUTCOME[c.outcome]}。${c.problems.map(line).join('；')}`)),
    '## 汇总（验收设计 §4.8）',
    '',
    ...summary(report, review),
    '## 覆盖点',
    '',
    ...table(['样本', '覆盖点', '字段', '结论', ...(review ? ['复核后'] : [])], report.samples.flatMap((s) => Object.entries(s.covers).map(([point, keys]) => [line(s.sample), line(point), keys.map(code).join('、'), STATE[s.coverage[point] ?? 'failed'], ...(review ? [STATE[review.coverage[s.sample]?.[point] ?? 'failed']] : [])]))),
    '## 严重错误',
    '',
    ...(review ? ['按复核后的结论列出。', ''] : []),
    '必答字段：',
    '',
    ...list(answers.filter(wrong).map(named)),
    '背景字段（不计入）：',
    '',
    ...list(fields.filter((f) => f.role === 'context' && wrong(f)).map(named)),
    ...(review ? ['复核改判为不算错的必答字段：', '', ...list(overturned('answer')), '复核改判为不算错的背景字段（不计入）：', '', ...list(overturned('context'))] : []),
    '`must_not_assert` 违规：',
    '',
    ...list(report.samples.flatMap((s) => s.must_not_assert.filter((m) => m.review?.verdict === 'violated').map((m) => `${line(s.sample)} · 第 ${m.n} 条：${line(m.proposition)}${m.review?.note ? `（${line(m.review.note)}）` : ''}`))),
    '## 待复核',
    '',
    '字段（严重的在前）：',
    '',
    ...list(waiting.map((f) => `${f.severity === 'critical' ? '**严重** ' : ''}${f.sample} · \`${f.key}\`：${said(f)}${f.score.candidates ? `；候选：${f.score.candidates.join('、')}` : ''}`)),
    '未覆盖的金标锚点：',
    '',
    ...list(uncovered),
    '`must_not_assert`（未评审的）：',
    '',
    ...list(report.samples.flatMap((s) => s.must_not_assert.filter((m) => m.review === undefined).map((m) => `${line(s.sample)} · 第 ${m.n} 条：${line(m.proposition)}${m.unless ? `（除非：${line(m.unless)}）` : ''}`))),
    ...(review && review.unused.length > 0 ? ['## 没有采用的复核条目', '', ...list(review.unused.map((u) => `${line(u.item.sample)} · ${line(String(u.item.field ?? `must_not_assert ${u.item.must_not_assert}`))}${u.item.anchor === undefined ? '' : ` · 锚点 ${u.item.anchor}`}：${line(u.why)}`))] : []),
  ].join('\n')
}
