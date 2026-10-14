import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { exitCode, main, parseArgs, writeEvidence } from './cli'
import { type Ran, type Report, buildReport } from './report'
import { run } from './run'
import { loadAsk } from './samples'
import { HOLIDAYS, NOTICE, type Odd, capabilities, daemon } from './test/daemon'
import { type Call, answer, failed, fakeClient, fakeFetch, id } from './test/fake'

const RAN: Ran = { commit: 'abc1234', machine: { model: 'Mac16,10', os: 'macOS 15.2' }, at: new Date('2026-10-14T08:00:00Z'), secrets: [] }
const both = [NOTICE, HOLIDAYS].map((sample) => ({ sample, ask: loadAsk(sample) }))
/** The report of a run on the stand-in; the job of the sample to be cancelled runs until then. */
const report = async (odd: Odd = {}, options: { cancelOn?: string; replayOn?: string } = {}): Promise<Report> =>
  buildReport(await run(fakeClient(daemon({ slow: options.cancelOn, ...odd })).client, both.slice(0, 1), { sleep: async () => {}, ...options }), [NOTICE, HOLIDAYS], RAN)

const made: string[] = []
const temp = (): string => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'accept-'))
  made.push(dir)
  return dir
}
afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
  for (const dir of made.splice(0)) fs.rmSync(dir, { recursive: true, force: true })
})

describe('parseArgs', () => {
  it('takes a run with only where to write, and names the samples of §3 for the idempotency checks', () => {
    const args = parseArgs(['run', '--out', 'evidence'])
    expect(args).toMatchObject({ command: 'run', out: 'evidence', replayOn: 's01-07-mixed-building-notice', cancelOn: 's01-01-zh-sh-fee-2025', jobMinutes: 30 })
    expect(fs.existsSync(path.join(args.samples, NOTICE.id, 'gold.json'))).toBe(true)
    expect(args.daemon).toBeUndefined()
    expect(args.only).toBeUndefined()
  })

  it('takes every option of a run', () => {
    const argv = ['run', '--samples', 's', '--daemon', 'd.json', '--only', 'a,b', '--replay-on', 'a', '--cancel-on', 'b', '--job-minutes', '5', '--out', 'o']
    expect(parseArgs(argv)).toEqual({ command: 'run', out: 'o', samples: 's', daemon: 'd.json', only: ['a', 'b'], replayOn: 'a', cancelOn: 'b', jobMinutes: 5 })
    expect(parseArgs(['review', '--out', 'o'])).toMatchObject({ command: 'review', out: 'o' })
    expect(parseArgs(['review', '--out', 'o', '--daemon', 'd.json'])).toMatchObject({ command: 'review', out: 'o', daemon: 'd.json' })
  })

  it('refuses what it does not know, with the usage', () => {
    const refused = (argv: string[], why: string) => expect(() => parseArgs(argv)).toThrow(new RegExp(`^${why}\\nusage: accept run --out <dir>`))
    refused([], 'nothing is no command')
    refused(['check', '--out', 'o'], 'check is no command')
    refused(['run'], '--out is needed: the directory of the evidence')
    refused(['review'], '--out is needed: the directory of the evidence')
    refused(['run', '--out'], '--out needs a value')
    refused(['run', '--out', ''], '--out needs a value')
    refused(['run', '--out', '--only', 'a'], '--out needs a value')
    refused(['run', '--out', 'o', '--verbose', '1'], '--verbose is no option of run')
    refused(['run', '--out', 'o', 'extra'], 'extra is no option of run')
    refused(['run', '--out', 'o', '--out', 'p'], '--out is given twice')
    refused(['review', '--out', 'o', '--only', 'a'], '--only is no option of review')
    for (const minutes of ['0', '-1', '1.5', 'ten', '1e1', '0x10', ' 5']) refused(['run', '--out', 'o', '--job-minutes', minutes], '--job-minutes needs a whole number of minutes')
  })
})

describe('exitCode', () => {
  it('is 0 only when every check passed, or had nothing to do', async () => {
    const all = await report({}, { replayOn: NOTICE.id, cancelOn: NOTICE.id })
    expect(exitCode(all)).toBe(0)
    const conclude = (check: string, conclusion: Report['trace'][number]['conclusion']): Report => ({ ...all, trace: all.trace.map((t) => (t.check === check ? { ...t, conclusion } : t)) })
    expect(exitCode(conclude('ACC-IDEM-3', 'skipped'))).toBe(0)
    for (const conclusion of ['failed', 'blocked', 'not_run', 'unmet'] as const) expect(exitCode(conclude('ACC-GET', conclusion)), conclusion).toBe(1)
    // A run without the idempotency checks is not the whole acceptance.
    expect(exitCode(await report())).toBe(1)
    expect(exitCode(await report({ ends: failed('engine_unavailable') }, { replayOn: NOTICE.id, cancelOn: NOTICE.id }))).toBe(1)
  })

  it('is 2 when the daemon could not run the checks: that is no failure of the product', async () => {
    expect(exitCode(await report({ capabilities: () => answer(capabilities(false)) }))).toBe(2)
    expect(exitCode(await report({ capabilities: () => answer({}) }))).toBe(1)
  })
})

describe('writeEvidence', () => {
  it('writes the report as JSON and for a person to read', async () => {
    const dir = path.join(temp(), 'new', 'evidence')
    const written = await report()
    writeEvidence(dir, written, ['', 'not-in-it'])
    expect(fs.readdirSync(dir).sort()).toEqual(['report.json', 'report.md'])
    expect(JSON.parse(fs.readFileSync(path.join(dir, 'report.json'), 'utf8'))).toEqual(JSON.parse(JSON.stringify(written)))
    expect(fs.readFileSync(path.join(dir, 'report.md'), 'utf8')).toMatch(/^# Documenting S01 验收报告\n[\s\S]*\n$/)
  })

  it('writes nothing when the report holds a secret, and does not say the secret', async () => {
    const dir = path.join(temp(), 'evidence')
    const clean = await report()
    const inJson: Report = { ...clean, samples: clean.samples.map((s) => ({ ...s, document_id: 'tok-123' })) }
    expect(() => writeEvidence(dir, inJson, ['absent', 'tok-123'])).toThrow('the report holds what it must not (secret 2 of 2): nothing was written')
    const inBoth: Report = { ...clean, environment: { ...clean.environment, commit: 'Mac-mini-X' } }
    let said = ''
    try {
      writeEvidence(dir, inBoth, ['Mac-mini-X'])
    } catch (error) {
      said = String(error)
    }
    expect(said).toContain('secret 1 of 1')
    expect(said).not.toContain('Mac-mini-X')
    // In another case, or as JSON writes it, it is the same secret.
    expect(() => writeEvidence(dir, inBoth, ['mac-MINI-x'])).toThrow('secret 1 of 1')
    const quoted: Report = { ...clean, samples: clean.samples.map((s) => ({ ...s, checks: [{ id: 'ACC-GET', outcome: 'failed' as const, problems: ['bad bearer tok"9\\f'], ms: 0 }] })) }
    expect(() => writeEvidence(dir, { ...quoted, samples: quoted.samples.map((s) => ({ ...s, document_id: 'tok"9\\f', checks: [] })) }, ['tok"9\\f'])).toThrow('secret 1 of 1')
    expect(fs.existsSync(dir)).toBe(false)
  })

  it('looks in report.md too: there white space is one space, and a home with a tab in report.json is said as it is', async () => {
    const dir = path.join(temp(), 'evidence')
    const clean = await report()
    for (const space of ['\t', '  ', '\n']) {
      const spaced: Report = { ...clean, environment: { ...clean.environment, machine: { model: `/srv2/My${space}Drive/dx`, os: 'macOS 15.2' } } }
      expect(() => writeEvidence(dir, spaced, ['/srv2/My Drive/dx']), JSON.stringify(space)).toThrow('secret 1 of 1')
    }
    expect(fs.existsSync(dir)).toBe(false)
  })

  it('writes nothing that holds a path of this machine, wherever it came from; a name with a slash is no path', async () => {
    const dir = path.join(temp(), 'evidence')
    const clean = await report()
    const model = (id: string): Report => ({ ...clean, environment: { ...clean.environment, model_ids: [id] }, samples: clean.samples.map((s) => ({ ...s, model_id: id })) })
    for (const local of ['/Volumes/Data/models/qwen', '/opt/models/qwen.gguf', '~/models/qwen']) {
      expect(() => writeEvidence(dir, model(local), []), local).toThrow('the report holds a path of this machine: nothing was written')
    }
    // report.md does not say the model of each sample: only report.json has it there.
    expect(() => writeEvidence(dir, { ...clean, samples: clean.samples.map((s) => ({ ...s, model_id: '/Volumes/Data/models/qwen' })) }, [])).toThrow('a path of this machine')
    expect(fs.existsSync(dir)).toBe(false)
    writeEvidence(dir, model('mlx-community/Qwen2.5-7B'), [])
    expect(fs.readFileSync(path.join(dir, 'report.md'), 'utf8')).toContain('mlx-community/Qwen2.5-7B')
  })

  it('takes a secret where it stands as a whole, not inside a longer name', async () => {
    const dir = path.join(temp(), 'evidence')
    const clean = await report()
    const on = (model: string): Report => ({ ...clean, environment: { ...clean.environment, machine: { model, os: 'macOS 15.2' } } })
    for (const said of ['MacBook', 'on macbook.local', 'MacBook-2,', '(macbook)']) expect(() => writeEvidence(dir, on(said), ['MacBook']), said).toThrow('secret 1 of 1')
    writeEvidence(dir, on('MacBookPro18,3'), ['MacBook'])
    writeEvidence(dir, on('iMacBook'), ['MacBook'])
    expect(fs.readdirSync(dir).sort()).toEqual(['report.json', 'report.md'])
  })

  it('finds a secret in whichever text of the report it is, and then writes nothing', async () => {
    const dir = path.join(temp(), 'evidence')
    const clean = JSON.parse(JSON.stringify(await report({}, { cancelOn: NOTICE.id, replayOn: NOTICE.id })))
    // Every place in the report that holds text.
    const places: (string | number)[][] = []
    const walk = (x: unknown, at: (string | number)[]) => {
      if (typeof x === 'string') places.push(at)
      else if (Array.isArray(x)) x.forEach((y, i) => walk(y, [...at, i]))
      else if (typeof x === 'object' && x !== null) for (const [k, y] of Object.entries(x)) walk(y, [...at, k])
    }
    walk(clean, [])
    expect(places.length).toBeGreaterThan(100)
    for (const at of places) {
      const planted = structuredClone(clean)
      const parent = at.slice(0, -1).reduce((x, k) => x[k], planted)
      parent[at.at(-1)!] = `${parent[at.at(-1)!]} on Mac-mini-Nicolas.local`
      expect(() => writeEvidence(dir, planted, ['s3cret-tok', 'mac-mini-nicolas']), at.join('.')).toThrow('secret 2 of 2')
    }
    expect(fs.existsSync(dir)).toBe(false)
  })
})

describe('main', () => {
  const lines = async (argv: string[]) => {
    const printed: string[] = []
    return { code: await main(argv, (line) => printed.push(line)), printed }
  }
  /** A daemon state file, and the daemon it names, answered by the stand-in. */
  const serve = (odd: Odd = {}): { state: string; calls: Call[] } => {
    const calls: Call[] = []
    vi.stubGlobal('fetch', fakeFetch(daemon(odd), calls))
    const state = path.join(temp(), 'daemon.json')
    fs.writeFileSync(state, JSON.stringify({ port: 1, token: 'tok-9f3a1c' }))
    return { state, calls }
  }

  it('runs the samples it was told to through the daemon, writes the evidence, and says how each check ended', async () => {
    const { state, calls } = serve({ slow: HOLIDAYS.id })
    const out = path.join(temp(), 'evidence')
    const { code, printed } = await lines(['run', '--out', out, '--daemon', state, '--only', `${NOTICE.id},${HOLIDAYS.id}`, '--cancel-on', HOLIDAYS.id])
    expect(printed.slice(0, 10).map((l) => l.split(/\s+/))).toEqual(['ACC-CAP', 'ACC-IMPORT', 'ACC-GET', 'ACC-TEXT', 'ACC-RENDER', 'ACC-FIND', 'ACC-EXTRACT', 'ACC-IDEM-1', 'ACC-IDEM-2', 'ACC-IDEM-3'].map((c) => [c, 'passed']))
    expect(printed[10]).toBe(`report.json and report.md are in ${out}`)
    expect(code).toBe(0)
    const written = JSON.parse(fs.readFileSync(path.join(out, 'report.json'), 'utf8')) as Report
    expect(written.samples.map((s) => s.sample)).toEqual([HOLIDAYS.id, NOTICE.id])
    expect(written.environment.commit).toMatch(/^[0-9a-f]{12}(-dirty)?$|^unknown$/)
    expect(written.environment.machine.model).not.toBe('')
    expect(calls.every((c) => c.headers.authorization === 'Bearer tok-9f3a1c')).toBe(true)
    const text = fs.readFileSync(path.join(out, 'report.json'), 'utf8') + fs.readFileSync(path.join(out, 'report.md'), 'utf8')
    for (const secret of ['tok-9f3a1c', os.hostname(), os.homedir()]) expect(text).not.toContain(secret)
  })

  it('exits with 1 when a check did not pass, and with 2 when the daemon could not run them; the evidence is written all the same', async () => {
    const failing = serve({ ends: failed('engine_unavailable') })
    const out = path.join(temp(), 'evidence')
    const failedRun = await lines(['run', '--out', out, '--daemon', failing.state, '--only', NOTICE.id])
    expect(failedRun.code).toBe(1)
    expect(failedRun.printed).toContain('ACC-EXTRACT  failed')
    expect(failedRun.printed).toContain('ACC-IDEM-1   blocked')
    expect(failedRun.printed).toContain('ACC-IDEM-3   not_run')
    expect(fs.existsSync(path.join(out, 'report.md'))).toBe(true)
    const unmet = serve({ capabilities: () => answer(capabilities(false)) })
    const elsewhere = path.join(temp(), 'evidence')
    expect((await lines(['run', '--out', elsewhere, '--daemon', unmet.state])).code).toBe(2)
    expect(unmet.calls.map((c) => c.route)).toEqual(['/capabilities'])
    expect((JSON.parse(fs.readFileSync(path.join(elsewhere, 'report.json'), 'utf8')) as Report).samples).toEqual([])
  })

  it('keeps the bearer out of the evidence when the daemon says it back', async () => {
    const echo = () => answer({ error: { code: 'parse_failed', message: 'no text for bearer tok-9f3a1c', details: { retryable: false } } }, 422)
    const { state } = serve({ text: echo })
    const out = path.join(temp(), 'evidence')
    await lines(['run', '--out', out, '--daemon', state, '--only', NOTICE.id])
    const json = fs.readFileSync(path.join(out, 'report.json'), 'utf8')
    expect(json).toContain('422 parse_failed: no text for bearer <redacted>')
    expect(json + fs.readFileSync(path.join(out, 'report.md'), 'utf8')).not.toContain('tok-9f3a1c')
  })

  it('keeps this machine out of the evidence: its name, with and without its domain, and the home, whatever the daemon says', async () => {
    vi.spyOn(os, 'hostname').mockReturnValue('Mac-mini-Nicolas.local')
    vi.spyOn(os, 'homedir').mockReturnValue('/srv2/dx')
    const echo = () => answer({ error: { code: 'parse_failed', message: 'no text on Mac-mini-Nicolas.local, Mac-mini-Nicolas, in /srv2/dx/x', details: { retryable: false } } }, 422)
    const { state } = serve({ text: echo })
    const out = path.join(temp(), 'evidence')
    await lines(['run', '--out', out, '--daemon', state, '--only', NOTICE.id])
    const json = fs.readFileSync(path.join(out, 'report.json'), 'utf8')
    expect(json).toContain('422 parse_failed: no text on <redacted>, <redacted>, in <redacted>/x')
    // What a review merges in is held to the same: then nothing is written.
    const before = json + fs.readFileSync(path.join(out, 'report.md'), 'utf8')
    fs.writeFileSync(path.join(out, 'review.json'), JSON.stringify({ reviewer: 'david', items: [{ sample: NOTICE.id, extraction_id: id('ext'), field: 'notice_reference', verdict: 'correct', note: 'read on mac-mini-nicolas' }] }))
    await expect(main(['review', '--out', out])).rejects.toThrow('the report holds what it must not')
    expect(fs.readFileSync(path.join(out, 'report.json'), 'utf8') + fs.readFileSync(path.join(out, 'report.md'), 'utf8')).toBe(before)
    // A short name says too little to be looked for: it would be found in any report.
    vi.spyOn(os, 'hostname').mockReturnValue('pass.local')
    const plain = path.join(temp(), 'evidence')
    expect((await lines(['run', '--out', plain, '--daemon', serve().state, '--only', NOTICE.id, '--cancel-on', NOTICE.id])).code).toBe(0)
    // So too without a domain, where the name is the whole of it.
    vi.spyOn(os, 'hostname').mockReturnValue('Mac16')
    const short = path.join(temp(), 'evidence')
    await lines(['run', '--out', short, '--daemon', serve({ text: () => answer({ error: { code: 'parse_failed', message: 'no text on Mac16', details: { retryable: false } } }, 422) }).state, '--only', NOTICE.id])
    expect(fs.readFileSync(path.join(short, 'report.json'), 'utf8')).toContain('no text on Mac16')
  })

  it('takes as samples only the directories that have a gold.json', async () => {
    const samples = temp()
    fs.cpSync(NOTICE.dir, path.join(samples, NOTICE.id), { recursive: true })
    fs.mkdirSync(path.join(samples, 'notes'))
    const { state } = serve()
    const out = path.join(temp(), 'evidence')
    expect((await lines(['run', '--out', out, '--daemon', state, '--samples', samples, '--cancel-on', NOTICE.id])).code).toBe(0)
    expect((JSON.parse(fs.readFileSync(path.join(out, 'report.json'), 'utf8')) as Report).samples.map((s) => s.sample)).toEqual([NOTICE.id])
  })

  it('refuses a sample it does not have, a directory without samples, and a daemon it cannot find, before it asks anything', async () => {
    const { state, calls } = serve()
    const out = path.join(temp(), 'evidence')
    await expect(main(['run', '--out', out, '--daemon', state, '--only', `${NOTICE.id},s01-99-nope`])).rejects.toThrow('no such sample: s01-99-nope')
    const empty = temp()
    await expect(main(['run', '--out', out, '--daemon', state, '--samples', empty])).rejects.toThrow(`${empty} has no samples`)
    await expect(main(['run', '--out', out, '--daemon', path.join(empty, 'none.json'), '--only', NOTICE.id])).rejects.toThrow('not a daemon state file that can be read')
    // A sample named for an idempotency check must be one that runs; the defaults need not be (the run above with --only had neither).
    await expect(main(['run', '--out', out, '--daemon', state, '--only', NOTICE.id, '--replay-on', HOLIDAYS.id])).rejects.toThrow('--replay-on must name a sample of this run')
    await expect(main(['run', '--out', out, '--daemon', state, '--only', NOTICE.id, '--cancel-on', 's01-99-nope', '--replay-on', 'x'])).rejects.toThrow('--replay-on and --cancel-on must name a sample of this run')
    expect(calls).toEqual([])
    expect(fs.existsSync(out)).toBe(false)
    // Each flag is held to its own sample: one that runs is taken, whatever the other's default.
    expect((await lines(['run', '--out', out, '--daemon', state, '--only', NOTICE.id, '--replay-on', NOTICE.id])).printed).toContain('ACC-IDEM-1   passed')
    // A job that had succeeded when the cancel came leaves nothing to check: skipped is no failure, and with all else passed the run exits 0.
    const fast = await lines(['run', '--out', out, '--daemon', state, '--only', NOTICE.id, '--cancel-on', NOTICE.id, '--replay-on', NOTICE.id])
    expect(fast.printed).toContain('ACC-IDEM-3   skipped')
    expect(fast.code).toBe(0)
    const running = serve({ slow: NOTICE.id })
    expect((await lines(['run', '--out', out, '--daemon', running.state, '--only', NOTICE.id, '--cancel-on', NOTICE.id])).printed).toContain('ACC-IDEM-3   passed')
  })

  it('merges review.json into the evidence, as often as it is asked, and says what it did not apply', async () => {
    const { state } = serve()
    const out = path.join(temp(), 'evidence')
    await lines(['run', '--out', out, '--daemon', state, '--only', NOTICE.id])
    const plain = fs.readFileSync(path.join(out, 'report.md'), 'utf8')
    expect(plain).toContain('还没有人工复核')
    const stale = { sample: NOTICE.id, extraction_id: id('ext', 7), field: 'work_order', verdict: 'correct' }
    const review = { reviewer: 'david', items: [{ sample: NOTICE.id, extraction_id: id('ext'), field: 'notice_reference', verdict: 'correct', note: '编号一致' }, stale] }
    fs.writeFileSync(path.join(out, 'review.json'), JSON.stringify(review))
    const first = await lines(['review', '--out', out])
    // What it did not apply is said by what it is about, not as it was written.
    expect(first).toEqual({ code: 0, printed: ['merged the review of david: 1 decisions applied, 1 not', `  not applied: ${NOTICE.id} · work_order (it is about extraction ${id('ext', 7)}, and the report is of ${id('ext')})`] })
    const merged = fs.readFileSync(path.join(out, 'report.md'), 'utf8')
    expect(merged).toContain('| 指标 | 必答字段 | 背景字段 | 必答字段（复核后） | 背景字段（复核后） |')
    expect(merged).toContain('- 人工复核：david，采用 1 条，未采用 1 条。')
    const json = fs.readFileSync(path.join(out, 'report.json'), 'utf8')
    expect((JSON.parse(json) as Report).samples[0]!.fields.find((f) => f.key === 'notice_reference')?.review).toEqual({ verdict: 'correct', reviewer: 'david', note: '编号一致' })
    // Asked again, it writes the same; with the decision taken back, the verdict is the machine's again.
    await lines(['review', '--out', out])
    expect(fs.readFileSync(path.join(out, 'report.json'), 'utf8')).toBe(json)
    fs.writeFileSync(path.join(out, 'review.json'), JSON.stringify({ reviewer: 'david', items: [] }))
    await lines(['review', '--out', out])
    expect((JSON.parse(fs.readFileSync(path.join(out, 'report.json'), 'utf8')) as Report).samples[0]!.fields.find((f) => f.key === 'notice_reference')?.review).toBeUndefined()
  })

  it('holds a review to the bearer too: of the daemon named, or else of the usual state file where there is one', async () => {
    const { state } = serve()
    const out = path.join(temp(), 'evidence')
    await lines(['run', '--out', out, '--daemon', state, '--only', NOTICE.id])
    const before = fs.readFileSync(path.join(out, 'report.json'), 'utf8')
    fs.writeFileSync(path.join(out, 'review.json'), JSON.stringify({ reviewer: 'david', items: [{ sample: NOTICE.id, extraction_id: id('ext'), field: 'notice_reference', verdict: 'correct', note: 'Bearer tok-9f3a1c' }] }))
    await expect(main(['review', '--out', out, '--daemon', state])).rejects.toThrow('the report holds what it must not (secret 1 of')
    await expect(main(['review', '--out', out, '--daemon', path.join(out, 'none.json')])).rejects.toThrow('not a daemon state file that can be read')
    // Without --daemon, ~/.agent24/daemon.json, where there is one.
    const home = temp()
    vi.spyOn(os, 'homedir').mockReturnValue(home)
    fs.mkdirSync(path.join(home, '.agent24'))
    fs.copyFileSync(state, path.join(home, '.agent24', 'daemon.json'))
    await expect(main(['review', '--out', out])).rejects.toThrow('the report holds what it must not (secret 1 of')
    expect(fs.readFileSync(path.join(out, 'report.json'), 'utf8')).toBe(before)
    fs.rmSync(path.join(home, '.agent24'), { recursive: true })
    expect((await lines(['review', '--out', out])).code).toBe(0)
  })

  it('refuses a review it cannot read, and leaves the evidence as it was', async () => {
    const { state } = serve()
    const out = path.join(temp(), 'evidence')
    await lines(['run', '--out', out, '--daemon', state, '--only', NOTICE.id])
    const before = fs.readFileSync(path.join(out, 'report.json'), 'utf8')
    await expect(main(['review', '--out', out])).rejects.toThrow('review.json')
    fs.writeFileSync(path.join(out, 'review.json'), JSON.stringify({ items: [] }))
    await expect(main(['review', '--out', out])).rejects.toThrow('review.json must have a reviewer and items')
    expect(fs.readFileSync(path.join(out, 'report.json'), 'utf8')).toBe(before)
  })

  it('refuses to merge into a report.json that is not the one it wrote', async () => {
    const { state } = serve()
    const out = path.join(temp(), 'evidence')
    await lines(['run', '--out', out, '--daemon', state, '--only', NOTICE.id])
    fs.writeFileSync(path.join(out, 'review.json'), JSON.stringify({ reviewer: 'david', items: [] }))
    const written = JSON.parse(fs.readFileSync(path.join(out, 'report.json'), 'utf8')) as Report
    const put = (changed: unknown) => fs.writeFileSync(path.join(out, 'report.json'), JSON.stringify(changed))
    put({ ...written, schema: 'something-else-v9' })
    await expect(main(['review', '--out', out])).rejects.toThrow('report.json is not a report of this harness')
    put({ ...written, totals: { ...written.totals, answer: { ...written.totals.answer, correct: written.totals.answer.fields } } })
    await expect(main(['review', '--out', out])).rejects.toThrow('report.json: its totals or coverage points do not follow from its fields')
    expect(fs.readFileSync(path.join(out, 'report.md'), 'utf8')).toContain('还没有人工复核')
    put(written)
    expect((await lines(['review', '--out', out])).code).toBe(0)
  })
})
