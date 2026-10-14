import { describe, expect, it } from 'vitest'
import { type SampleRun, run, scoreSample } from './run'
import { loadAsk } from './samples'
import type { Extracted } from './score'
import { HOLIDAYS, IMPORT, JOB, NOTICE, type Odd, RERUN, capabilities, daemon } from './test/daemon'
import { anchor, answer, block, extraction, failed, fakeClient, id, refuse, textPage } from './test/fake'

const QUICK = { sleep: async () => {} }
const READS = ['ACC-IMPORT', 'ACC-GET', 'ACC-TEXT', 'ACC-RENDER', 'ACC-FIND']

const both = [NOTICE, HOLIDAYS].map((sample) => ({ sample, ask: loadAsk(sample) }))
const one = both.slice(0, 1)
/** A run on the stand-in. The job of the sample to be cancelled runs until then, unless `odd` says otherwise. */
const go = async (odd: Odd = {}, samples = one, options: { cancelOn?: string; replayOn?: string; timeoutMs?: number } = {}) => {
  const { client, calls } = fakeClient(daemon({ slow: options.cancelOn, ...odd }))
  const result = await run(client, samples, { ...QUICK, ...options })
  return { result, calls, did: calls.map((c) => `${c.method} ${c.route.replace(/doc_\w+/, 'DOC')}`) }
}
const outcomes = (checks: { id: string; outcome: string }[]) => checks.map((c) => `${c.id} ${c.outcome}`)

describe('run', () => {
  it('runs every check of a sample in the order of §3, and scores every gold field', async () => {
    const { result } = await go()
    expect(result.capabilities).toMatchObject({ id: 'ACC-CAP', outcome: 'passed' })
    expect(result.unverified).toEqual([])
    expect(result.samples).toHaveLength(1)
    const notice = result.samples[0]!
    expect(notice).toMatchObject({ sample: NOTICE.id, documentId: id('doc'), text: { blocks: 1, partial: false }, find: { anchors: expect.any(Number) }, extraction: { extractionId: id('ext') } })
    expect(notice.checks.flatMap((c) => c.problems)).toEqual([])
    expect(outcomes(notice.checks)).toEqual([...READS, 'ACC-EXTRACT'].map((c) => `${c} passed`))
    expect(notice.checks.every((c) => c.sample === NOTICE.id)).toBe(true)
    expect(notice.scored.map((s) => s.gold.key)).toEqual(NOTICE.gold.fields.map((f) => f.key))
    expect(Object.keys(notice.coverage)).toEqual(Object.keys(NOTICE.meta.covers))
    expect(result.totals.answer.fields + result.totals.context.fields).toBe(NOTICE.gold.fields.length)
  })

  it('scores what the extraction said, each value against the gold of its own key', async () => {
    const said = (key: string) => {
      const gold = NOTICE.gold.fields.find((f) => f.key === key)!
      return gold.status === 'present' && key === 'work_order' ? { key, status: 'present', value: gold.value, anchors: [anchor(1, 'p1/b1', 0, 4, { content_sha256: `sha256:${NOTICE.meta.sha256_stored}` })] } : { key, status: 'missing', missing_reason: 'not_in_document' }
    }
    const { result } = await go({ values: loadAsk(NOTICE).map((f) => said(f.key)) })
    const verdict = (key: string) => result.samples[0]!.scored.find((s) => s.gold.key === key)?.score.verdict
    expect(verdict('work_order')).toBe('correct')
    expect(verdict('notice_reference')).toBe('missed')
    expect(verdict('shutoff_date')).toBe('missed')
    expect(verdict('contact_phone')).toBe('correct') // the notice gives none
    // Its anchor is where find put the gold quote: covered by place, not by a guess at the quote.
    expect(result.samples[0]!.scored.find((s) => s.gold.key === 'work_order')?.anchors).toMatchObject({ verdict: 'all', anchors: [{ by: 'place', unlocated: false }] })
    expect(result.totals.context.correct).toBe(1)
  })

  it('tells the scoring when the text layer had regions that were not read', async () => {
    // "Unread" stands for "not in the document" only when part of the document was not read.
    const values = loadAsk(NOTICE).map((f) => ({ key: f.key, status: 'missing', missing_reason: 'unread' }))
    const phone = async (text?: () => Response) => (await go({ text, values })).result.samples[0]!.scored.find((s) => s.gold.key === 'contact_phone')?.score
    const partial = () => answer(textPage([block('p1/b1')], null, { parse_status: 'partial', unparsed_regions: [{ page: 1, reason: 'ocr_failed' }] }))
    expect(await phone()).toEqual({ key: 'contact_phone', verdict: 'correct', reasonAgrees: false })
    expect(await phone(partial)).toEqual({ key: 'contact_phone', verdict: 'correct', reasonAgrees: true })
  })

  it('runs the samples one after the other, each as its own document, and adds them up', async () => {
    const { result, did } = await go({}, both)
    expect(result.samples.map((s) => [s.sample, s.documentId])).toEqual([[NOTICE.id, id('doc', 1)], [HOLIDAYS.id, id('doc', 2)]])
    for (const s of result.samples) expect(outcomes(s.checks)).toEqual([...READS, 'ACC-EXTRACT'].map((c) => `${c} passed`))
    expect(result.totals.answer.fields + result.totals.context.fields).toBe(NOTICE.gold.fields.length + HOLIDAYS.gold.fields.length)
    expect(did.filter((d) => d === 'GET /capabilities')).toHaveLength(1)
    expect(did.indexOf('POST /uploads')).toBeLessThan(did.lastIndexOf('POST /uploads'))
    expect(did.lastIndexOf('POST /uploads')).toBeGreaterThan(did.indexOf(`GET /extractions/${id('ext')}`))
  })

  it('leaves out a sample whose gold nobody has verified, and says so', async () => {
    const draft = { sample: { ...NOTICE, gold: { ...NOTICE.gold, verified_by: null as never } }, ask: one[0]!.ask }
    const { result, did } = await go({}, [draft, both[1]!])
    expect(result.unverified).toEqual([NOTICE.id])
    expect(result.samples.map((s) => s.sample)).toEqual([HOLIDAYS.id])
    expect(did.filter((d) => d === 'POST /uploads')).toHaveLength(1)
  })

  it('asks nothing more of a daemon that cannot do what the checks need, or does not say', async () => {
    const unmet = await go({ capabilities: () => answer(capabilities(false)) }, both)
    expect(unmet.result).toMatchObject({ capabilities: { outcome: 'unmet' }, samples: [], unverified: [] })
    expect(unmet.result.totals.answer.fields).toBe(0)
    expect(unmet.did).toEqual(['GET /capabilities'])
    const silent = await go({ capabilities: () => refuse(404, 'not_found') }, both)
    expect(silent.result).toMatchObject({ capabilities: { outcome: 'failed' }, samples: [] })
    expect(silent.did).toEqual(['GET /capabilities'])
    const wrong = await go({ capabilities: () => answer({ ...capabilities(), storage: 'ready' }) }, both)
    expect(wrong.result).toMatchObject({ capabilities: { outcome: 'failed' }, samples: [] })
  })
})

describe('run, when a check does not pass', () => {
  const all = { replayOn: NOTICE.id, cancelOn: NOTICE.id }
  /** A sample nothing was read of: every gold field is there, without an answer, and no coverage point passed. */
  const unread = (s: SampleRun): boolean => {
    const sample = [NOTICE, HOLIDAYS].find((x) => x.id === s.sample)!
    expect(s.scored.map((f) => [f.gold.key, f.score.verdict])).toEqual(sample.gold.fields.map((f) => [f.key, f.status === 'missing' ? 'unanswered' : 'missed']))
    expect(s.scored.every((f) => f.anchors === undefined)).toBe(true)
    expect(s.coverage).toEqual(Object.fromEntries(Object.keys(sample.meta.covers).map((point) => [point, 'failed'])))
    return s.extraction === undefined
  }

  it('counts a sample whose extraction was not read in every total, each of its fields as without an answer', async () => {
    let uploads = 0
    const theDaemon = daemon()
    const { client } = fakeClient((call) => {
      if (call.route === '/uploads') uploads += 1
      // Only the job of the second sample fails.
      return uploads === 2 && call.route === `/jobs/${JOB}` ? answer(failed('engine_unavailable')) : theDaemon(call)
    })
    const result = await run(client, both, QUICK)
    const [notice, holidays] = result.samples
    expect(outcomes(holidays!.checks).at(-1)).toBe('ACC-EXTRACT failed')
    expect(unread(holidays!)).toBe(true)
    expect(notice!.scored.some((f) => f.score.verdict === 'correct')).toBe(true)
    const { answer: answers, context } = result.totals
    expect(answers.fields + context.fields).toBe(NOTICE.gold.fields.length + HOLIDAYS.gold.fields.length)
    const critical = HOLIDAYS.gold.fields.filter((f) => f.severity === 'critical' && f.role !== 'context')
    expect(critical.length).toBeGreaterThan(0)
    expect(answers.critical.filter((c) => c.sample === HOLIDAYS.id).map((c) => c.key)).toEqual(critical.map((f) => f.key))
    // With no sample read at all, the totals are of every field, none correct.
    const none = (await go({ ends: failed('engine_unavailable') }, both)).result.totals
    expect(none.answer.fields + none.context.fields).toBe(NOTICE.gold.fields.length + HOLIDAYS.gold.fields.length)
    expect(none.answer.correct + none.context.correct).toBe(0)
  })

  it('blocks ACC-EXTRACT, and does not fail it, when ACC-IDEM-3 left the job cancelled: there is nothing to read', async () => {
    const { result, did } = await go({ retry: () => refuse(409, 'job_not_retryable') }, one, all)
    const checks = result.samples[0]!.checks
    expect(outcomes(checks).slice(-4)).toEqual(['ACC-IDEM-3 failed', 'ACC-EXTRACT blocked', 'ACC-IDEM-1 blocked', 'ACC-IDEM-2 blocked'])
    expect(checks.slice(-3).map((c) => c.problems)).toEqual(Array(3).fill(['ACC-IDEM-3 left the job cancelled']))
    expect(did.some((d) => d.includes('/extractions/'))).toBe(false)
    expect(unread(result.samples[0]!)).toBe(true)
    // Without the replays to block, only ACC-EXTRACT is.
    expect(outcomes((await go({ retry: () => refuse(409, 'job_not_retryable') }, one, { cancelOn: NOTICE.id })).result.samples[0]!.checks).slice(-2)).toEqual(['ACC-IDEM-3 failed', 'ACC-EXTRACT blocked'])
  })

  it('blocks ACC-EXTRACT at once when the cancel left the job stopping and it has not stopped: it is not waited for again', async () => {
    const { result, did } = await go({ running: 'stuck' }, one, { ...all, timeoutMs: 0 })
    const checks = result.samples[0]!.checks
    expect(outcomes(checks).slice(-4)).toEqual(['ACC-IDEM-3 failed', 'ACC-EXTRACT blocked', 'ACC-IDEM-1 blocked', 'ACC-IDEM-2 blocked'])
    expect(checks.find((c) => c.id === 'ACC-IDEM-3')?.problems).toEqual([`job ${JOB} is still cancelling`])
    expect(checks.slice(-3).map((c) => c.problems)).toEqual(Array(3).fill(['ACC-IDEM-3 left the job cancelling']))
    // Read before the cancel, polled once by ACC-IDEM-3, and looked at once more: no third wait.
    expect(did.filter((d) => d === `GET /jobs/${JOB}`)).toHaveLength(3)
    expect(result.samples[0]).toMatchObject({ interrupted: true })
  })

  it('reads the extraction when ACC-IDEM-3 failed before it cancelled anything', async () => {
    let read = 0
    const theDaemon = daemon()
    const { client, calls } = fakeClient((call) => (call.route === `/jobs/${JOB}` && (read += 1) === 1 ? refuse(503, 'storage_unavailable') : theDaemon(call)))
    const notice = (await run(client, one, { ...QUICK, cancelOn: NOTICE.id })).samples[0]!
    expect(outcomes(notice.checks).slice(-2)).toEqual(['ACC-IDEM-3 failed', 'ACC-EXTRACT passed'])
    expect(calls.some((c) => c.route.endsWith('/cancel'))).toBe(false)
    expect(notice).not.toHaveProperty('interrupted')
  })

  it('still reads the extraction when ACC-IDEM-3 failed and the job went on all the same', async () => {
    const theDaemon = daemon({ slow: NOTICE.id })
    const { client } = fakeClient((call) => {
      const real = theDaemon(call)
      // The retry queues the job, but answers as a replay would.
      return call.route.endsWith('/retry') ? new Response(real.body, { status: 202, headers: real.headers }) : real
    })
    const notice = (await run(client, one, { ...QUICK, ...all })).samples[0]!
    expect(outcomes(notice.checks).slice(-4)).toEqual(['ACC-IDEM-3 failed', 'ACC-EXTRACT passed', 'ACC-IDEM-1 passed', 'ACC-IDEM-2 passed'])
    expect(notice.checks.find((c) => c.id === 'ACC-IDEM-3')?.problems).toEqual(['the retry answered 202, not 200'])
    expect(notice.extraction).toBeDefined()
  })

  it('blocks every later check of a sample that was not imported, and goes on to the next sample', async () => {
    let imports = 0
    const { client, calls } = fakeClient((call) => {
      // Only the first import fails.
      if (call.route === `/jobs/${IMPORT}` && (imports += 1) === 1) return answer(failed('unsupported_format', { job_id: IMPORT, kind: 'import' }))
      return theDaemon(call)
    })
    const theDaemon = daemon()
    const result = await run(client, both, { ...QUICK, ...all })
    const [notice, holidays] = result.samples
    expect(outcomes(notice!.checks)).toEqual(['ACC-IMPORT failed', 'ACC-GET blocked', 'ACC-TEXT blocked', 'ACC-RENDER blocked', 'ACC-FIND blocked', 'ACC-IDEM-3 blocked', 'ACC-EXTRACT blocked', 'ACC-IDEM-1 blocked', 'ACC-IDEM-2 blocked'])
    expect(notice!.checks.slice(1).every((c) => c.problems.join() === 'the sample was not imported' && c.sample === NOTICE.id)).toBe(true)
    expect(unread(notice!)).toBe(true)
    expect(notice!.documentId).toBeUndefined()
    expect(outcomes(holidays!.checks)).toEqual([...READS, 'ACC-EXTRACT'].map((c) => `${c} passed`))
    expect(calls.filter((c) => c.route.endsWith('/extractions'))).toHaveLength(1)
  })

  it('blocks only the checks a sample was to have', async () => {
    const { result } = await go({ imported: failed('unsupported_format', { job_id: IMPORT, kind: 'import' }) })
    expect(outcomes(result.samples[0]!.checks)).toEqual(['ACC-IMPORT failed', 'ACC-GET blocked', 'ACC-TEXT blocked', 'ACC-RENDER blocked', 'ACC-FIND blocked', 'ACC-EXTRACT blocked'])
  })

  it('goes on to extract when the text or find could not be read, and scores without them', async () => {
    const { result } = await go({ text: () => refuse(422, 'parse_failed'), find: () => refuse(503, 'engine_unavailable') })
    const notice = result.samples[0]!
    expect(outcomes(notice.checks)).toEqual(['ACC-IMPORT passed', 'ACC-GET passed', 'ACC-TEXT failed', 'ACC-RENDER passed', 'ACC-FIND failed', 'ACC-EXTRACT passed'])
    expect(notice.text).toBeUndefined()
    expect(notice.find).toBeUndefined()
    expect(notice.scored).toHaveLength(NOTICE.gold.fields.length)
  })

  it('fails ACC-EXTRACT when the extraction is refused, and blocks the checks that need its job', async () => {
    const { result, did } = await go({ start: () => refuse(400, 'invalid_request') }, one, all)
    const checks = result.samples[0]!.checks
    expect(outcomes(checks)).toEqual([...READS.map((c) => `${c} passed`), 'ACC-IDEM-3 blocked', 'ACC-EXTRACT failed', 'ACC-IDEM-1 blocked', 'ACC-IDEM-2 blocked'])
    expect(checks.find((c) => c.id === 'ACC-EXTRACT')?.problems).toEqual(['400 invalid_request: it is invalid_request'])
    expect(checks.filter((c) => c.outcome === 'blocked').map((c) => c.problems.join())).toEqual(Array(3).fill('the extraction was not started'))
    expect(did.filter((d) => d.includes('/jobs/job_') || d.includes('/extractions/'))).toEqual([`GET /jobs/${IMPORT}`])
    expect(unread(result.samples[0]!)).toBe(true)
  })

  it('does not replay a job whose extraction was not read: the replay would run it again', async () => {
    const { result, did } = await go({ ends: failed('engine_unavailable') }, one, { replayOn: NOTICE.id })
    const checks = result.samples[0]!.checks
    expect(outcomes(checks).slice(-3)).toEqual(['ACC-EXTRACT failed', 'ACC-IDEM-1 blocked', 'ACC-IDEM-2 blocked'])
    expect(checks.slice(-2).map((c) => c.problems)).toEqual([['no extraction was read to replay'], ['no extraction was read to replay']])
    expect(did.filter((d) => d === 'POST /documents/DOC/extractions')).toHaveLength(1)
    expect(unread(result.samples[0]!)).toBe(true)
    expect(result.samples[0]!.extraction).toBeUndefined()
    // An extraction that was read is replayed and scored, though it left a question out.
    const short = await go({ values: extraction(one[0]!.ask.slice(1).map((f) => f.key), null).values }, one, { replayOn: NOTICE.id })
    expect(outcomes(short.result.samples[0]!.checks).slice(-3)).toEqual(['ACC-EXTRACT failed', 'ACC-IDEM-1 passed', 'ACC-IDEM-2 passed'])
    expect(short.result.samples[0]!.scored).toHaveLength(NOTICE.gold.fields.length)
  })
})

describe('run, with the idempotency checks', () => {
  it('replays and reruns the extraction of one sample, after it has been read', async () => {
    const { result, did } = await go({}, both, { replayOn: NOTICE.id })
    expect(outcomes(result.samples[0]!.checks)).toEqual([...READS, 'ACC-EXTRACT', 'ACC-IDEM-1', 'ACC-IDEM-2'].map((c) => `${c} passed`))
    expect(outcomes(result.samples[1]!.checks)).toEqual([...READS, 'ACC-EXTRACT'].map((c) => `${c} passed`))
    const extraction = did.indexOf(`GET /extractions/${id('ext')}`)
    const starts = did.flatMap((d, i) => (d === 'POST /documents/DOC/extractions' ? [i] : []))
    // The first of one sample: started once, then the replay and three reruns after its values were read.
    const first = starts.filter((i) => i < did.lastIndexOf('POST /uploads'))
    expect(first).toHaveLength(5)
    expect(first[0]).toBeLessThan(extraction)
    expect(first[1]).toBeGreaterThan(extraction)
    expect(did).toContain(`POST /jobs/${RERUN}/cancel`)
    expect(did).not.toContain(`POST /jobs/${JOB}/cancel`)
  })

  it('cancels the extraction of one sample before it is read, and reads the attempt the retry queued', async () => {
    const { result, did } = await go({}, one, { cancelOn: NOTICE.id })
    const notice = result.samples[0]!
    expect(outcomes(notice.checks)).toEqual([...READS, 'ACC-IDEM-3', 'ACC-EXTRACT'].map((c) => `${c} passed`))
    const at = (what: string) => did.indexOf(what)
    expect(at('POST /documents/DOC/extractions')).toBeLessThan(at(`POST /jobs/${JOB}/cancel`))
    expect(at(`POST /jobs/${JOB}/cancel`)).toBeLessThan(at(`POST /jobs/${JOB}/retry`))
    expect(at(`POST /jobs/${JOB}/retry`)).toBeLessThan(at(`GET /extractions/${id('ext')}`))
    expect(notice.scored).toHaveLength(NOTICE.gold.fields.length)
  })

  it('has nothing to cancel when the job had succeeded: ACC-IDEM-3 is skipped, and the extraction is read as it is', async () => {
    const { result, did } = await go({ slow: undefined }, one, { cancelOn: NOTICE.id, replayOn: NOTICE.id })
    const notice = result.samples[0]!
    expect(outcomes(notice.checks).slice(-4)).toEqual(['ACC-IDEM-3 skipped', 'ACC-EXTRACT passed', 'ACC-IDEM-1 passed', 'ACC-IDEM-2 passed'])
    expect(notice).not.toHaveProperty('interrupted')
    expect(did).not.toContain(`POST /jobs/${JOB}/retry`)
    expect(notice.scored).toHaveLength(NOTICE.gold.fields.length)
  })

  it('does both on one sample: cancel, read, then replay', async () => {
    const { result } = await go({}, one, { replayOn: NOTICE.id, cancelOn: NOTICE.id })
    expect(outcomes(result.samples[0]!.checks)).toEqual([...READS, 'ACC-IDEM-3', 'ACC-EXTRACT', 'ACC-IDEM-1', 'ACC-IDEM-2'].map((c) => `${c} passed`))
  })

  it('says which samples the idempotency checks were asked for, and what they met there', async () => {
    const none = (await go()).result
    expect(none.idempotency).toEqual({})
    expect(none.samples[0]).not.toHaveProperty('requeued')
    expect(none.samples[0]).not.toHaveProperty('interrupted')
    const asked = (await go({}, one, { replayOn: NOTICE.id, cancelOn: HOLIDAYS.id })).result
    expect(asked.idempotency).toEqual({ replayOn: NOTICE.id, cancelOn: HOLIDAYS.id })
    // The stand-in's job had succeeded when it was replayed, and was not running when it was cancelled.
    expect(asked.samples[0]).toMatchObject({ requeued: false })
    expect(asked.samples[0]).not.toHaveProperty('interrupted')
    expect((await go({}, one, { cancelOn: NOTICE.id })).result.samples[0]).toMatchObject({ interrupted: false })
    const running = (await go({ running: 'stops' }, one, { cancelOn: NOTICE.id })).result.samples[0]!
    expect(running).toMatchObject({ interrupted: true })
    expect(outcomes(running.checks).slice(-2)).toEqual(['ACC-IDEM-3 passed', 'ACC-EXTRACT passed'])
  })

  it('keeps of every check only its result: what the check read has its own place in the run', async () => {
    const { result } = await go({}, both, { replayOn: NOTICE.id, cancelOn: NOTICE.id })
    const checks = result.samples.flatMap((s) => s.checks)
    expect(checks).toHaveLength(9 + 6)
    for (const c of checks) expect(Object.keys(c).sort(), c.id).toEqual(['id', 'ms', 'outcome', 'problems', 'sample'])
    expect(result.samples[0]).toMatchObject({ documentId: id('doc', 1), text: { blocks: 1 }, find: { reachable: expect.any(Number) }, extraction: { extractionId: id('ext') } })
  })

  it('does neither on a sample that was not named', async () => {
    const { did } = await go({}, one, { replayOn: HOLIDAYS.id, cancelOn: HOLIDAYS.id })
    expect(did.filter((d) => d === 'POST /documents/DOC/extractions')).toHaveLength(1)
    expect(did.some((d) => d.endsWith('/cancel') || d.endsWith('/retry'))).toBe(false)
  })
})

describe('scoreSample', () => {
  it('scores a key answered more than once by its worst answer, whatever order they came in', () => {
    const field = NOTICE.gold.fields.find((f) => f.status === 'present' && f.value && f.normalized)!
    const said = (value: string, normalized?: string): Extracted => ({ key: field.key, status: 'present', value, normalized, anchors: [] })
    const right = said(field.value!, field.normalized)
    const unsure = said('—', field.normalized)
    const wrong = said('—', '—')
    const verdict = (...values: Extracted[]) => scoreSample(NOTICE, values, undefined, false).find((s) => s.gold.key === field.key)?.score.verdict
    expect([verdict(), verdict(right), verdict(unsure), verdict(wrong)]).toEqual(['missed', 'correct', 'review', 'mismatch'])
    expect(verdict(right, right)).toBe('correct')
    for (const [a, b] of [[right, unsure], [unsure, right]]) expect(verdict(a!, b!)).toBe('review')
    for (const [a, b] of [[right, wrong], [wrong, right], [unsure, wrong], [wrong, unsure]]) expect(verdict(a!, b!)).toBe('mismatch')
    // Of two answers as bad as each other the first is scored; the anchors are those of the answer that is scored.
    const absent: Extracted = { key: field.key, status: 'missing', missing_reason: 'not_in_document' }
    expect([verdict(wrong, absent), verdict(absent, wrong)]).toEqual(['mismatch', 'missed'])
    expect(scoreSample(NOTICE, [right, absent], undefined, false).find((s) => s.gold.key === field.key)?.result).toEqual(absent)
  })

  it('scores the anchors of a key answered more than once by the answer that covers the fewest, whatever order they came in', () => {
    const field = NOTICE.gold.fields.find((f) => f.status === 'present' && f.value && f.normalized && (f.anchors?.length ?? 0) > 0)!
    const gold = field.anchors![0]!
    const sourced = [{ ...anchor(gold.page, 'p1/b1'), quote: gold.quote }]
    const said = (value: string, anchors: object[]): Extracted => ({ key: field.key, status: 'present', value, normalized: field.normalized, anchors } as Extracted)
    const covering = said(field.value!, sourced)
    const bare = said(field.value!, [])
    const wrong = { ...said('—', sourced), normalized: '—' }
    const absent: Extracted = { key: field.key, status: 'missing', missing_reason: 'not_in_document' }
    const anchors = (...values: Extracted[]) => scoreSample(NOTICE, values, undefined, false).find((s) => s.gold.key === field.key)?.anchors
    const covered = (...values: Extracted[]) => anchors(...values)?.anchors.filter((a) => a.by !== undefined).length
    expect([covered(covering), covered(bare), anchors(absent)]).toEqual([1, 0, undefined])
    // The verdict is wrong's, the anchors bare's: neither answer makes the coverage better.
    for (const twice of [[bare, wrong], [wrong, bare], [covering, bare], [bare, covering]]) expect(anchors(...twice)?.verdict, JSON.stringify(twice.map((t) => t.anchors?.length))).toBe('unsourced')
    // An answer with no value to anchor does not take away the anchors of one that has.
    for (const twice of [[covering, absent], [absent, covering]]) expect(covered(...twice)).toBe(1)
    for (const twice of [[bare, absent], [absent, bare]]) expect(covered(...twice)).toBe(0)
  })

  it('of two answers that cover as few, scores the anchors of the one with more gold anchors to cover', () => {
    // shutoff_end_time: 4 gold anchors stand on 18:00, 1 on 17:00.
    const only = (normalized: string): Extracted => ({ key: 'shutoff_end_time', status: 'conflict', candidates: [{ value: normalized, normalized, anchors: [] }] } as Extracted)
    const gold = (...values: Extracted[]) => scoreSample(NOTICE, values, undefined, false).find((s) => s.gold.key === 'shutoff_end_time')?.anchors?.anchors.length
    expect([gold(only('18:00')), gold(only('17:00'))]).toEqual([4, 1])
    for (const twice of [[only('18:00'), only('17:00')], [only('17:00'), only('18:00')]]) expect(gold(...twice)).toBe(4)
  })

  it('compares the anchors of two answers by the share of gold anchors they cover, not by how many', () => {
    // A cites the one gold quote of 17:00 (1 of 1); B cites two of the four of 18:00 (2 of 4), and the two it
    // leaves out are neither inside nor around a quote it cites.
    const field = NOTICE.gold.fields.find((f) => f.key === 'shutoff_end_time')!
    const cites = (normalized: string, which: number[]): Extracted => {
      const quotes = field.candidates!.filter((c) => c.normalized === normalized).flatMap((c) => c.anchors).filter((_, i) => which.includes(i))
      return { key: field.key, status: 'conflict', candidates: [{ value: normalized, normalized, anchors: quotes.map((q) => ({ ...anchor(q.page, 'p1/b1'), quote: q.quote })) }] } as Extracted
    }
    const share = (...values: Extracted[]) => {
      const a = scoreSample(NOTICE, values, undefined, false).find((s) => s.gold.key === field.key)!.anchors!.anchors
      return `${a.filter((x) => x.by !== undefined).length}/${a.length}`
    }
    const [a, b] = [cites('17:00', [0]), cites('18:00', [0, 2])]
    expect([share(a), share(b)]).toEqual(['1/1', '2/4'])
    expect([share(a, b), share(b, a)]).toEqual(['2/4', '2/4'])
  })
})
