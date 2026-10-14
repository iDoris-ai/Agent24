import crypto from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { describe, expect, it } from 'vitest'
import type { CheckResult } from './checks'
import { type Ran, buildReport, scrub } from './report'
import { type Run, run } from './run'
import { loadAsk } from './samples'
import { HOLIDAYS, NOTICE, type Odd, capabilities, daemon } from './test/daemon'
import { anchor, answer, failed, fakeClient, id, refuse } from './test/fake'

const RAN: Ran = { commit: 'abc1234', machine: { model: 'Mac16,10', os: 'macOS 15.2' }, at: new Date('2026-10-14T08:00:00Z'), secrets: ['s3cret-token', 'Mac-mini-Nicolas'] }
const SAMPLES = [NOTICE, HOLIDAYS]
const both = SAMPLES.map((sample) => ({ sample, ask: loadAsk(sample) }))
/** A run on the stand-in; the job of the sample to be cancelled runs until then. */
const ran = async (odd: Odd = {}, samples = both.slice(0, 1), options: { cancelOn?: string; replayOn?: string } = {}): Promise<Run> => run(fakeClient(daemon({ slow: options.cancelOn, ...odd })).client, samples, { sleep: async () => {}, ...options })
const fileSha = (file: string) => `sha256:${crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')}`

describe('buildReport', () => {
  it('says what ran, on what, and with which model', async () => {
    const report = buildReport(await ran({}, both), SAMPLES, RAN)
    expect(report).toMatchObject({ schema: 'documents-accept-report-v1', generated_at: '2026-10-14T08:00:00.000Z', unverified: [] })
    expect(report.environment).toEqual({
      commit: 'abc1234',
      machine: { model: 'Mac16,10', os: 'macOS 15.2' },
      os_version: '0.1.0',
      engines: [{ id: 'apple-pdfkit', kind: 'parse', state: 'ready', version: '15.2.0' }, { id: 'apple-pdfkit', kind: 'render', state: 'ready', version: '15.2.0' }],
      model_ids: ['qwen2.5:7b'],
      extractor_versions: ['2'],
    })
    expect(report.capabilities).toEqual({ id: 'ACC-CAP', outcome: 'passed', problems: [], ms: expect.any(Number) })
    // An engine that is not there is said to be so: it explains what a scan could not give.
    const noOcr = { ...capabilities(), engines: [...capabilities().engines, { id: 'apple-vision', kind: 'ocr', formats: [], state: 'absent', version: null }] }
    expect(buildReport(await ran({ capabilities: () => answer(noOcr) }), SAMPLES, RAN).environment.engines[2]).toEqual({ id: 'apple-vision', kind: 'ocr', state: 'absent', version: null })
    expect(report.samples.map((s) => s.sample)).toEqual([NOTICE.id, HOLIDAYS.id])
  })

  it('names every sample by the bytes that were run: the source, the gold and the questions', async () => {
    const [notice] = buildReport(await ran(), SAMPLES, RAN).samples
    expect(notice).toMatchObject({
      sample: NOTICE.id,
      content_sha256: `sha256:${NOTICE.meta.sha256_stored}`,
      gold_sha256: fileSha(path.join(NOTICE.dir, 'gold.json')),
      ask_sha256: fileSha(path.join(NOTICE.dir, 'ask.json')),
      document_id: id('doc'),
      extraction_id: id('ext'),
      model_id: 'qwen2.5:7b',
      extractor_version: '2',
    })
    expect(notice!.gold_sha256).not.toBe(notice!.ask_sha256)
    expect(notice!.checks.map((c) => `${c.id} ${c.outcome}`)).toEqual(['ACC-IMPORT', 'ACC-GET', 'ACC-TEXT', 'ACC-RENDER', 'ACC-FIND', 'ACC-EXTRACT'].map((c) => `${c} passed`))
    expect(notice!.checks.every((c) => typeof c.ms === 'number')).toBe(true)
    expect(notice!.text).toEqual({ textLayer: expect.any(String), blocks: 1, partial: false, unparsed: [] })
    expect(Object.keys(notice!.coverage)).toEqual(Object.keys(NOTICE.meta.covers))
    expect(notice!.covers).toEqual(NOTICE.meta.covers)
  })

  it('sets every gold field beside what the extraction said for it, and the verdict', async () => {
    const order = { key: 'work_order', status: 'present', value: 'WO-000731', anchors: [anchor(1, 'p1/b1', 0, 4, { content_sha256: `sha256:${NOTICE.meta.sha256_stored}` })] }
    const values = loadAsk(NOTICE).map((f) => (f.key === 'work_order' ? order : { key: f.key, status: 'missing', missing_reason: 'not_in_document' }))
    const [notice] = buildReport(await ran({ values }), SAMPLES, RAN).samples
    expect(notice!.fields.map((f) => f.key)).toEqual(NOTICE.gold.fields.map((f) => f.key))
    const field = (key: string) => notice!.fields.find((f) => f.key === key)!
    expect(field('work_order')).toMatchObject({
      role: 'context',
      severity: 'critical',
      gold: { status: 'present', value: 'WO-000731' },
      result: order,
      score: { key: 'work_order', verdict: 'correct' },
      anchors: { verdict: 'all', anchors: [{ gold: { page: 1, quote: '工单号 Work Order: WO-000731' }, by: 'place', unlocated: false }] },
    })
    expect(field('contact_phone')).toMatchObject({ role: 'answer', gold: { status: 'missing', missing_reason: 'not_in_document' }, result: { status: 'missing' }, score: { verdict: 'correct', reasonAgrees: true } })
    expect(field('contact_phone').anchors).toBeUndefined()
    // A conflict in the gold: its candidates as they are said, without their anchors.
    const gold = NOTICE.gold.fields.find((f) => f.key === 'shutoff_date')!
    expect(field('shutoff_date')).toMatchObject({ score: { verdict: 'missed' } })
    expect(field('shutoff_date').gold).toEqual({ status: 'conflict', candidates: gold.candidates!.map(({ value, normalized }) => ({ value, normalized })) })
  })

  it('sets beside a key answered twice the answer that was scored, whichever came first', async () => {
    const at = [anchor(1, 'p1/b1', 0, 4, { content_sha256: `sha256:${NOTICE.meta.sha256_stored}` })]
    const order = { key: 'work_order', status: 'present', value: 'WO-000731', anchors: at }
    const wrong = { key: 'work_order', status: 'present', value: 'WO-999999', normalized: 'WO-999999', anchors: at }
    for (const twice of [[order, wrong], [wrong, order]]) {
      const values = [...loadAsk(NOTICE).filter((f) => f.key !== 'work_order').map((f) => ({ key: f.key, status: 'missing', missing_reason: 'not_in_document' })), ...twice]
      const [notice] = buildReport(await ran({ values }), SAMPLES, RAN).samples
      expect(notice!.fields.find((f) => f.key === 'work_order')).toMatchObject({ result: wrong, score: { verdict: 'mismatch' } })
    }
  })

  it('lists what the gold forbids to assert, numbered from 1 and not yet judged', async () => {
    const [notice] = buildReport(await ran(), SAMPLES, RAN).samples
    const gold = NOTICE.gold.must_not_assert
    expect(notice!.must_not_assert).toHaveLength(gold.length)
    expect(notice!.must_not_assert[0]).toEqual({ n: 1, proposition: gold[0]!.proposition, verdict: 'unreviewed' })
    const sixth = notice!.must_not_assert[5]!
    expect(sixth).toEqual({ n: 6, proposition: gold[5]!.proposition, unless: gold[5]!.unless, verdict: 'unreviewed' })
    expect(sixth.unless).toBeDefined()
  })

  it('counts the gold anchors find reached, and lists the quotes it did not reach, with why', async () => {
    const lost = '工单号 Work Order: WO-000731'
    const unread = { page: 1, reason: 'ocr_failed' }
    let asked = 0
    const find = () => ((asked += 1) === 1 ? refuse(400, 'invalid_request') : answer({ parse_status: 'partial', unparsed_regions: [unread], matches: [], next_cursor: null }))
    const report = buildReport(await ran({ find }), SAMPLES, RAN)
    const reached = report.samples[0]!.find!
    expect(reached.reachable).toBe(0)
    expect(reached.anchors).toBeGreaterThan(reached.unreached.length)
    expect(reached.unreached[0]).toEqual({ page: 1, quote: NOTICE.gold.fields[0]!.anchors![0]!.quote, refused: 'invalid_request' })
    expect(reached.unreached.find((u) => u.quote === lost)).toEqual({ page: 1, quote: lost, unparsed: [unread] })
    expect(report.find).toEqual({ anchors: reached.anchors, reachable: 0, unread: 0 })
    // Over two samples the numbers add up; what find reached is not listed.
    const two = buildReport(await ran({}, both), SAMPLES, RAN)
    const [notice, holidays] = two.samples.map((s) => s.find!)
    expect(two.find).toEqual({ anchors: notice!.anchors + holidays!.anchors, reachable: notice!.reachable + holidays!.reachable, unread: 0 })
    expect(notice).toMatchObject({ reachable: notice!.anchors, unreached: [] })
    // The stand-in finds every quote on page 1 only.
    expect(holidays!.reachable).toBeLessThan(holidays!.anchors)
    expect(holidays!.unreached.every((u) => u.page > 1)).toBe(true)
  })

  it('counts the gold anchors of a sample whose find was not read as not reached, and says how many such samples there are', async () => {
    let uploads = 0
    const theDaemon = daemon()
    const { client } = fakeClient((call) => {
      if (call.route === '/uploads') uploads += 1
      return uploads === 2 && call.route.endsWith('/find') ? refuse(503, 'engine_unavailable') : theDaemon(call)
    })
    const report = buildReport(await run(client, both, { sleep: async () => {} }), SAMPLES, RAN)
    const [notice, holidays] = report.samples
    expect(holidays!.find).toBeUndefined()
    expect(holidays!.checks.find((c) => c.id === 'ACC-FIND')?.outcome).toBe('failed')
    const gold = HOLIDAYS.gold.fields.reduce((n, f) => n + (f.anchors?.length ?? 0) + (f.candidates ?? []).reduce((m, c) => m + c.anchors.length, 0), 0)
    expect(gold).toBeGreaterThan(0)
    expect(report.find).toEqual({ anchors: notice!.find!.anchors + gold, reachable: notice!.find!.reachable, unread: 1 })
    // Counted as find itself counts them, where it was read: the anchors of the candidates of a conflict too.
    const read = buildReport(await ran({}, both), SAMPLES, RAN)
    expect(read.samples[1]!.find!.anchors).toBe(gold)
    const unread = buildReport(await ran({ find: () => refuse(503, 'engine_unavailable') }), SAMPLES, RAN)
    expect(unread.find).toEqual({ anchors: read.samples[0]!.find!.anchors, reachable: 0, unread: 1 })
  })

  it('reports a sample that was not read with every field, none answered, and no extraction to review', async () => {
    const [notice] = buildReport(await ran({ ends: failed('engine_unavailable') }), SAMPLES, RAN).samples
    expect(notice!.fields.map((f) => f.key)).toEqual(NOTICE.gold.fields.map((f) => f.key))
    expect(notice!.fields.every((f) => f.result === undefined && f.anchors === undefined && (f.score.verdict === 'missed' || f.score.verdict === 'unanswered'))).toBe(true)
    expect(notice).not.toHaveProperty('extraction_id', expect.anything())
    expect(Object.values(notice!.coverage)).toEqual(Object.keys(NOTICE.meta.covers).map(() => 'failed'))
  })

  it('says where the idempotency checks were asked for, and what they met there', async () => {
    const report = buildReport(await ran({}, both, { replayOn: NOTICE.id, cancelOn: NOTICE.id }), SAMPLES, RAN)
    expect(report.idempotency).toEqual({ replayOn: NOTICE.id, cancelOn: NOTICE.id })
    expect(report.samples[0]).toMatchObject({ requeued: false, interrupted: false })
    expect(JSON.parse(JSON.stringify(report.samples[1]))).not.toHaveProperty('requeued')
    expect(JSON.parse(JSON.stringify(report.samples[1]))).not.toHaveProperty('interrupted')
    expect(buildReport(await ran(), SAMPLES, RAN).idempotency).toEqual({})
    // Of a check there is its result and nothing else: what it read is not written twice.
    for (const c of report.samples.flatMap((s) => s.checks)) expect(Object.keys(c).sort(), c.id).toEqual(['id', 'ms', 'outcome', 'problems', 'sample'])
  })

  it('hands the totals of the run on, and refuses a run with a sample it was not given', async () => {
    const done = await ran({}, both)
    expect(buildReport(done, SAMPLES, RAN).totals).toEqual(done.totals)
    // A sample left out for its unverified gold is named, and is not among those that ran.
    const draft = { sample: { ...NOTICE, gold: { ...NOTICE.gold, verified_by: null as never } }, ask: both[0]!.ask }
    expect(buildReport(await ran({}, [draft, both[1]!]), SAMPLES, RAN)).toMatchObject({ unverified: [NOTICE.id], samples: [{ sample: HOLIDAYS.id }] })
    expect(() => buildReport(done, [NOTICE], RAN)).toThrow(`the run has a sample that was not given: ${HOLIDAYS.id}`)
  })

  it('reports a sample that was not imported, and one whose extraction ACC-IDEM-3 left cancelled, as not read', async () => {
    const lost = buildReport(await ran({ imported: failed('unsupported_format', { job_id: id('job', 9), kind: 'import' }) }), SAMPLES, RAN)
    expect(lost.find).toEqual({ anchors: buildReport(await ran(), SAMPLES, RAN).find.anchors, reachable: 0, unread: 1 })
    expect(JSON.parse(JSON.stringify(lost.samples[0]))).not.toHaveProperty('document_id')
    const stopped = buildReport(await ran({ retry: () => refuse(409, 'job_not_retryable') }, both.slice(0, 1), { cancelOn: NOTICE.id }), SAMPLES, RAN)
    expect(JSON.parse(JSON.stringify(stopped.samples[0]))).not.toHaveProperty('extraction_id')
    expect(stopped.samples[0]!.fields.every((f) => f.result === undefined)).toBe(true)
    expect(stopped.trace.find((t) => t.check === 'ACC-EXTRACT')).toMatchObject({ conclusion: 'blocked', results: { blocked: 1 } })
    expect(stopped.trace.find((t) => t.check === 'ACC-IDEM-3')).toMatchObject({ conclusion: 'failed' })
  })

  it('says of a run on a daemon that could not do it only that', async () => {
    const report = buildReport(await ran({ capabilities: () => answer(capabilities(false)) }, both), SAMPLES, RAN)
    expect(report).toMatchObject({ capabilities: { outcome: 'unmet' }, samples: [], find: { anchors: 0, reachable: 0, unread: 0 }, environment: { os_version: '0.1.0', model_ids: [], extractor_versions: [] } })
    expect(report.trace.map((t) => t.conclusion)).toEqual(['unmet', ...Array(9).fill('not_run')])
  })
})

describe('the trace chain', () => {
  const CHECKS = ['ACC-CAP', 'ACC-IMPORT', 'ACC-GET', 'ACC-TEXT', 'ACC-RENDER', 'ACC-FIND', 'ACC-EXTRACT', 'ACC-IDEM-1', 'ACC-IDEM-2', 'ACC-IDEM-3']
  const conclusions = (report: { trace: { check: string; conclusion: string }[] }) => Object.fromEntries(report.trace.map((t) => [t.check, t.conclusion]))

  it('has one row for every check of §3, in its order, with what it is evidence for', async () => {
    const report = buildReport(await ran({}, both, { replayOn: NOTICE.id, cancelOn: HOLIDAYS.id }), SAMPLES, RAN)
    expect(report.trace.map((t) => t.check)).toEqual(CHECKS)
    expect(report.trace.every((t) => t.conclusion === 'passed' && t.owner === 'David' && t.phase === 'DOC-1' && t.scenario !== '' && t.contract.startsWith('ADR-DOC-02 §'))).toBe(true)
    expect(report.trace.find((t) => t.check === 'ACC-EXTRACT')).toMatchObject({ scenario: 'A5, C1–C10, README §11.1/4/7/8', contract: 'ADR-DOC-02 §3.2', results: { passed: 2 } })
    expect(report.trace.find((t) => t.check === 'ACC-IDEM-3')?.results).toEqual({ passed: 1 })
    expect(report.trace.find((t) => t.check === 'ACC-CAP')?.results).toEqual({ passed: 1 })
  })

  it('concludes a check from all its samples: one failure fails it, and a check nobody ran was not run', async () => {
    let finds = 0
    const report = buildReport(await ran({ find: () => ((finds += 1) > 1 ? refuse(503, 'engine_unavailable') : refuse(400, 'invalid_request')) }, both), SAMPLES, RAN)
    expect(conclusions(report)).toMatchObject({ 'ACC-TEXT': 'passed', 'ACC-FIND': 'failed', 'ACC-EXTRACT': 'passed', 'ACC-IDEM-1': 'not_run', 'ACC-IDEM-3': 'not_run' })
    expect(report.trace.find((t) => t.check === 'ACC-FIND')?.results).toEqual({ failed: 2 })
    expect(report.trace.find((t) => t.check === 'ACC-IDEM-1')?.results).toEqual({})
  })

  it('does not conclude that a check passed when it failed, was blocked, or was only skipped somewhere', async () => {
    const done = await ran({}, both, { replayOn: NOTICE.id })
    const withOutcomes = (id: string, outcomes: CheckResult['outcome'][]): Run => {
      const samples = done.samples.map((s, i) => ({ ...s, checks: s.checks.filter((c) => c.id !== id).concat(outcomes[i] ? [{ id, sample: s.sample, outcome: outcomes[i], problems: [], ms: 0 }] : []) }))
      return { ...done, samples }
    }
    const of = (outcomes: CheckResult['outcome'][]) => conclusions(buildReport(withOutcomes('ACC-GET', outcomes), SAMPLES, RAN))['ACC-GET']
    expect(of(['passed', 'passed'])).toBe('passed')
    expect(of(['passed', 'failed'])).toBe('failed')
    expect(of(['failed', 'blocked'])).toBe('failed')
    expect(of(['passed', 'blocked'])).toBe('blocked')
    expect(of(['skipped', 'blocked'])).toBe('blocked')
    expect(of(['unmet', 'passed'])).toBe('unmet')
    expect(of(['passed', 'skipped'])).toBe('passed')
    expect(of(['skipped', 'skipped'])).toBe('skipped')
    expect(of(['failed', 'skipped'])).toBe('failed')
    expect(of(['unmet', 'blocked'])).toBe('blocked')
    expect(of([])).toBe('not_run')
  })
})

describe('what a report must not hold (§5)', () => {
  it('takes the secrets and the paths of the machine out of a problem', () => {
    expect(scrub('401 unauthorized: bad token s3cret-token from Mac-mini-Nicolas', RAN.secrets)).toBe('401 unauthorized: bad token <redacted> from <redacted>')
    expect(scrub("ENOENT: no such file, open '/Users/davidxu/.agent24/daemon.json'", [])).toBe("ENOENT: no such file, open '<path>'")
    expect(scrub('cannot read /Volumes/UltraDisk/Dev2/x.pdf, nor /private/tmp/y; see /var/log/z.', [])).toBe('cannot read <path>, nor <path>; see <path>.')
    expect(scrub('open C:\\Users\\david\\x.pdf failed', [])).toBe('open <path> failed')
    for (const where of ['~/.agent24/daemon.json', '/home/runner/x', '/root/x', '/tmp/x', '/opt/x/y', '/etc/hosts', '/usr/local/bin/x', '/mnt/d/x', '/srv/x', '/Library/Caches/x', '/Applications/X.app/y', '/System/Library/x']) {
      expect(scrub(`cannot open ${where}: denied`, []), where).toBe('cannot open <path>: denied')
    }
    // A path ends where the sentence goes on, in whatever punctuation.
    expect(scrub('读不了 /Users/x/y.pdf，稍后重试；另见 /tmp/z。完', [])).toBe('读不了 <path>，稍后重试；另见 <path>。完')
    expect(scrub('（/Users/x/y.pdf）“/tmp/z”', [])).toBe('（<path>）“<path>”')
    // What is no path of a machine stays: a route, a ratio, a pointer into an answer, the path of a URL, a cursor.
    const plain = 'GET /documents/doc_1/revisions/1/text answered 9/10: /values/0/key must match; see https://example.com/usr/share/x and cursor xY/tmp/Zq=='
    expect(scrub(plain, ['', 's3cret-token'])).toBe(plain)
    expect(scrub('see file:///Users/x/y.pdf', [])).toBe('see file://<path>')
    // The home is a secret and the start of a path: the whole path goes, not its start alone.
    const home = '/Users/davidxu'
    expect(scrub(`engine failed on ${home}/Dev/Agent24/docs/s01/source.pdf`, ['tok', 'host', home])).toBe('engine failed on <path>')
    expect(scrub(`home is ${home}`, [home])).toBe('home is <path>')
    // A directory's name may have a space in it; the next word of the sentence is no part of the path.
    expect(scrub(`ENOENT: open '${home}/Library/Application Support/agent24/data/doc_1/r1.pdf'`, [home])).toBe("ENOENT: open '<path>'")
    expect(scrub('open /Library/Application Support/agent24/My Data/r1.pdf failed', [])).toBe('open <path> failed')
    expect(scrub('cannot read /tmp/a.pdf nor /tmp/b.pdf at all', [])).toBe('cannot read <path> nor <path> at all')
    // In any case, with its slashes escaped as JSON may write them, and from the roots of other systems.
    expect(scrub('open /users/x/y.pdf and /VOLUMES/z', [])).toBe('open <path> and <path>')
    expect(scrub('{"path":"\\/Users\\/x\\/y.pdf"}', [])).toBe('{"path":"<path>"}')
    expect(scrub('{"url":"https:\\/\\/a.com\\/Users\\/x"}', [])).toBe('{"url":"https:\\/\\/a.com\\/Users\\/x"}')
    // A drive after a backslash is a drive all the same; a path after an escaped character of JSON is a path.
    expect(scrub('open \\\\?\\C:\\Users\\j\\x.pdf and x\\C:\\Users\\j failed', [])).toBe('open \\\\?\\<path> and x\\<path> failed')
    expect(scrub('build 2.C:\\x and abc:\\x', [])).toBe('build 2.C:\\x and abc:\\x')
    expect(scrub('{"error":"cannot read\\n/Users/j/x.pdf, nor\\t/tmp/y"}', [])).toBe('{"error":"cannot read\\n<path>, nor\\t<path>"}')
    expect(scrub('see /nix/store/abc-x, /run/user/501/s and /data/x.', [])).toBe('see <path>, <path> and <path>.')
    expect(scrub('<home /tmp/x>', [])).toBe('<home <path>>')
    // A secret in another case, or with its slashes escaped, is the same secret; one that only looks like a pattern is taken as it is.
    expect(scrub('at mac-mini-nicolas.local and MAC-MINI-NICOLAS', ['Mac-mini-Nicolas'])).toBe('at <redacted>.local and <redacted>')
    expect(scrub('{"path":"\\/users\\/DavidXu\\/x"}', [home])).toBe('{"path":"<path>"}')
    expect(scrub('{"who":"Team\\/A and team/a"}', ['team/a'])).toBe('{"who":"<redacted> and <redacted>"}')
    expect(scrub('a.b and a+b and axb', ['a.b', 'a+b'])).toBe('<redacted> and <redacted> and axb')
  })

  it('writes no problem down as the daemon gave it, and nothing of where the samples lie', async () => {
    const leaky = (where: string) => answer({ error: { code: 'parse_failed', message: `cannot read ${where} as s3cret-token`, details: { retryable: false } } }, 422)
    const report = buildReport(await ran({ text: () => leaky('/Users/davidxu/Library/x.pdf') }), SAMPLES, RAN)
    expect(report.samples[0]!.checks.find((c) => c.id === 'ACC-TEXT')?.problems).toEqual(['422 parse_failed: cannot read <path> as <redacted>'])
    const unmet = buildReport(await ran({ capabilities: () => leaky('/Volumes/UltraDisk/y') }), SAMPLES, RAN)
    expect(unmet.capabilities.problems).toEqual(['422 parse_failed: cannot read <path> as <redacted>'])
    for (const written of [JSON.stringify(report), JSON.stringify(unmet), JSON.stringify(buildReport(await ran({}, both), SAMPLES, RAN))]) {
      expect(written).not.toContain('s3cret-token')
      expect(written).not.toContain(NOTICE.dir)
      expect(written).not.toMatch(/\/(Users|Volumes|private)\//)
    }
  })

  it('keeps the daemon capabilities out of the check, where the report has its own place for them', async () => {
    const report = buildReport(await ran(), SAMPLES, RAN)
    expect(Object.keys(report.capabilities).sort()).toEqual(['id', 'ms', 'outcome', 'problems'])
  })
})
