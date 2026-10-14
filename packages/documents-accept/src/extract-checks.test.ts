import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { checkCancelledStays, checkExtract, checkReplay, checkRerunKey, startExtraction } from './extract-checks'
import { type AskField, loadSample } from './samples'
import { type Call, anchor, answer, extracted, extraction, failed, fakeClient, id, job, refuse, sha } from './test/fake'

const S01 = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..', 'docs', 'documenting', 'samples', 's01')
const NOTICE = loadSample(path.join(S01, 's01-07-mixed-building-notice'))
const ASK: AskField[] = [
  { key: 'shutoff_date', description: '哪一天停水', type: 'date' },
  { key: 'contact_phone', description: '联系电话是多少', type: 'identifier' },
]
const QUICK = { sleep: async () => {} }
const DOC = id('doc')
const JOB = id('job')
const RERUN = id('job', 2)
const EXT = id('ext')
const EXTRACTIONS = `/documents/${DOC}/extractions`
const NOT_A = (schema: string) => expect.stringContaining(`the answer is not a ${schema}`)
type Body = { revision: number; schema: { fields: AskField[] }; rerun?: boolean }
const STARTED = job() as never
const NOT_OURS = `job ${JOB} is not an extraction of revision 1 of this document`

describe('startExtraction', () => {
  it('asks for revision 1 with the questions as the schema, and nothing else', async () => {
    const { client, calls } = fakeClient(() => answer(job(), 202))
    expect((await startExtraction(client, DOC, ASK)).job_id).toBe(JOB)
    expect(calls).toHaveLength(1)
    expect(calls[0]).toMatchObject({ method: 'POST', route: EXTRACTIONS, json: { revision: 1, schema: { fields: ASK } } })
    expect(calls[0]?.headers['idempotency-key']).toBeUndefined()
  })

  it('refuses an answer the route does not give: a status that does not go with the job, or a job that is not this extraction', async () => {
    const start = (body: object, http: number) => startExtraction(fakeClient(() => answer(body, http)).client, DOC, ASK)
    await expect(start(job(), 200)).rejects.toThrow('the extraction route answered 200 with a job that is queued')
    await expect(start(job(), 201)).rejects.toThrow('the extraction route answered 201 with a job that is queued')
    await expect(start(extracted(), 202)).rejects.toThrow('the extraction route answered 202 with a job that is succeeded')
    await expect(start(job({ status: 'cancelled' }), 202)).rejects.toThrow('the extraction route answered 202 with a job that is cancelled')
    for (const http of [200, 202]) await expect(start(failed(), http)).rejects.toThrow(`the extraction route answered ${http} with a job that is failed`)
    for (const http of [200, 202]) await expect(start(job({ status: 'interrupted' }), http)).rejects.toThrow(`the extraction route answered ${http} with a job that is interrupted`)
    for (const other of [{ kind: 'export' }, { document_id: id('doc', 2) }, { revision: 2 }]) await expect(start(job(other), 202)).rejects.toThrow(NOT_OURS)
    expect((await start(job({ status: 'running', document_id: DOC, revision: 1 }), 202)).status).toBe('running')
    expect((await start(job({ status: 'cancelling' }), 202)).status).toBe('cancelling')
    for (const status of ['running', 'cancelling']) await expect(start(job({ status }), 200)).rejects.toThrow(`the extraction route answered 200 with a job that is ${status}`)
    expect((await start(extracted(), 200)).status).toBe('succeeded')
  })
})

describe('ACC-EXTRACT', () => {
  const QUESTIONS: AskField[] = NOTICE.gold.fields.map((f) => ({ key: f.key, description: 'x' }))
  const asked = QUESTIONS.map((q) => q.key)
  /** A page of the extraction of the sample's own bytes. */
  const page = (keys: string[], cursor: string | null, more: object = {}) => extraction(keys, cursor, { content_sha256: `sha256:${NOTICE.meta.sha256_stored}`, ...more })
  const two = (keys = asked, more: object = {}): object[] => [page(keys.slice(0, 2), 'c1', more), page(keys.slice(2), null, more)]
  const daemon = (ended: object = extracted(), pagesOf = two()) => (call: Call) => {
    if (call.route === `/jobs/${JOB}`) return answer(ended)
    return answer(pagesOf[call.route === `/extractions/${EXT}` ? 0 : 1])
  }
  const run = async (d: (call: Call) => Response, ask = QUESTIONS) => checkExtract(fakeClient(d).client, NOTICE, DOC, ask, STARTED, QUICK)

  it('waits for the job, then reads the values to the last page', async () => {
    const { client, calls } = fakeClient(daemon())
    const result = await checkExtract(client, NOTICE, DOC, QUESTIONS, STARTED, QUICK)
    expect(result).toMatchObject({ id: 'ACC-EXTRACT', sample: NOTICE.id, outcome: 'passed', problems: [] })
    expect(result.extraction).toMatchObject({ extractionId: EXT, modelId: 'qwen2.5:7b', extractorVersion: '2' })
    expect(result.extraction?.values.map((v) => v.key)).toEqual(asked)
    expect(calls.map((c) => c.route)).toEqual([`/jobs/${JOB}`, `/extractions/${EXT}`, `/extractions/${EXT}?cursor=c1`])
  })

  it('fails when the job does not succeed, with the reason the job gives', async () => {
    const lost = await run(daemon(failed('engine_unavailable')))
    expect(lost).toMatchObject({ outcome: 'failed', problems: ['the extraction job is failed: engine_unavailable'] })
    expect(lost.extraction).toBeUndefined()
    expect((await run(daemon(job({ status: 'cancelled' })))).problems).toEqual(['the extraction job is cancelled: no error code'])
  })

  it('fails on a job or an extraction that is not what the contract says', async () => {
    expect((await run(daemon(job({ status: 'done' })))).problems).toEqual([NOT_A('DocumentsJob')])
    expect((await run(daemon(job({ status: 'succeeded', result: {} })))).problems).toEqual([NOT_A('DocumentsJob')])
    expect((await run(daemon(job({ status: 'succeeded' })))).problems).toEqual([NOT_A('DocumentsJob')])
    const one = (more: object) => daemon(extracted(), [page(asked, null, more)])
    expect((await run(one({ model_id: '' }))).problems).toEqual([NOT_A('DocumentsExtraction')])
    expect((await run(one({ model_id: undefined }))).problems).toEqual([NOT_A('DocumentsExtraction')])
    expect((await run(one({ values: [{ key: asked[0], status: 'present' }] }))).problems).toEqual([NOT_A('DocumentsExtraction')])
    expect((await run(one({ values: 'none' }))).problems).toEqual([NOT_A('DocumentsExtraction')])
  })

  it('fails when the job is not the extraction of revision 1 of this document', async () => {
    expect((await run(daemon(extracted({ kind: 'export' })))).problems).toEqual([NOT_OURS])
    expect((await run(daemon(extracted({ document_id: id('doc', 2) })))).problems).toEqual([NOT_OURS])
    expect((await run(daemon(extracted({ revision: 2 })))).problems).toEqual([NOT_OURS])
    expect((await run(daemon(failed('engine_unavailable', { kind: 'export' })))).problems).toEqual([NOT_OURS])
    // Nor when its result says it is of another document or revision, though the job itself says nothing or the right thing.
    for (const other of [{ document_id: id('doc', 2) }, { revision: 2 }]) {
      expect((await run(daemon(extracted({ result: { extraction_id: EXT, ...other } })))).problems, JSON.stringify(other)).toEqual([NOT_OURS])
      expect((await run(daemon(extracted({ document_id: DOC, revision: 1, result: { extraction_id: EXT, ...other } })))).problems).toEqual([NOT_OURS])
    }
    expect((await run(daemon(extracted({ result: { extraction_id: EXT, document_id: DOC, revision: 1 } })))).problems).toEqual([])
    expect((await run(daemon(extracted({ document_id: DOC, revision: 1 })))).problems).toEqual([])
  })

  it('fails when the pages are not of one extraction, or do not end', async () => {
    const [first, second] = two()
    const other = { extraction_id: id('ext', 2), document_id: id('doc', 2), revision: 2, content_sha256: sha('c'), schema_sha256: sha('c'), extractor_version: '3', model_id: 'another', created_at: '2026-10-15T00:00:00.000Z' }
    for (const [key, value] of Object.entries(other)) {
      const result = await run(daemon(extracted(), [first!, { ...second, [key]: value }]))
      expect(result.problems).toEqual([`the pages of values are not of one extraction: ${key} changed`])
      expect(result.extraction).toBeUndefined()
    }
    expect((await run(daemon(extracted(), [page([], 'c1'), page([], 'c1')]))).problems).toEqual(['the pages do not end: cursor c1 came twice'])
  })

  it('fails when the extraction is another than the job names, or of another document, revision or content', async () => {
    const problems = async (more: object) => {
      const result = await run(daemon(extracted(), two(asked, more)))
      expect(result.extraction).toBeUndefined()
      return result.problems
    }
    expect(await problems({ extraction_id: id('ext', 2) })).toEqual([`asked for extraction ${EXT}, answered with ${id('ext', 2)}`])
    expect(await problems({ document_id: id('doc', 2) })).toEqual([`the extraction is of ${id('doc', 2)} revision 1, not of ${DOC} revision 1`])
    expect(await problems({ revision: 2 })).toEqual([`the extraction is of ${DOC} revision 2, not of ${DOC} revision 1`])
    expect(await problems({ content_sha256: sha('c') })).toEqual([`the extraction is of ${sha('c')}, not the sample's sha256_stored`])
  })

  it('fails when an anchor of a value, or of one side of a conflict, is not in revision 1 of this document', async () => {
    const here = (page: number, block: string, more: object = {}) => anchor(page, block, 0, 4, { content_sha256: `sha256:${NOTICE.meta.sha256_stored}`, ...more })
    const present = (more: object = {}) => ({ key: asked[0], status: 'present', value: 'x', anchors: [here(1, 'p1/b1'), here(1, 'p1/b2', more)] })
    const side = (more: object = {}) => ({ value: 'x', normalized: 'x', anchors: [here(1, 'p1/b1', more)] })
    const conflict = (more: object = {}) => ({ key: asked[1], status: 'conflict', candidates: [side(), side(more)] })
    const others = page(asked.slice(2), null).values
    const problems = async (values: object[]) => {
      const result = await run(daemon(extracted(), [page([], null, { values: [...values, ...others] })]))
      if (result.problems.length > 0) expect(result.extraction).toBeUndefined()
      return result.problems
    }
    const astray = (n: number) => [`${n} of the anchors are not in revision 1 of this document: another document, revision or content, or a page past the last`]
    expect(await problems([present(), conflict()])).toEqual([])
    for (const other of [{ document_id: id('doc', 2) }, { revision: 2 }, { content_sha256: sha('c') }, { page: 2 }]) {
      expect(await problems([present(other), conflict()]), JSON.stringify(other)).toEqual(astray(1))
      expect(await problems([present(), conflict(other)]), JSON.stringify(other)).toEqual(astray(1))
    }
    expect(await problems([present({ revision: 3 }), conflict({ page: 9 })])).toEqual(astray(2))
  })

  it('fails when a question has no answer or two, or an answer has no question, and still hands the values on to be scored', async () => {
    const short = await run(daemon(extracted(), two(asked.slice(0, -1))))
    expect(short).toMatchObject({ outcome: 'failed', problems: [`no answer to: ${asked.at(-1)}`] })
    expect(short.extraction?.values).toHaveLength(asked.length - 1)
    expect((await run(daemon(extracted(), two([])))).problems).toEqual([`no answer to: ${asked.join(', ')}`])
    expect((await run(daemon(extracted(), two([...asked, asked[0]!])))).problems).toEqual([`answered twice, or never asked: ${asked[0]}`])
    expect((await run(daemon(extracted(), two([...asked, 'never_asked'])))).problems).toEqual(['answered twice, or never asked: never_asked'])
    // As many answers as questions is not enough: one twice in place of another, or one nobody asked.
    expect((await run(daemon(extracted(), two([...asked.slice(0, -1), asked[0]!])))).problems).toEqual([`no answer to: ${asked.at(-1)}`, `answered twice, or never asked: ${asked[0]}`])
    expect((await run(daemon(extracted(), two([...asked.slice(1), 'never_asked'])))).problems).toEqual([`no answer to: ${asked[0]}`, 'answered twice, or never asked: never_asked'])
    // In another order they are the same answers.
    expect((await run(daemon(extracted(), two([...asked].reverse())))).problems).toEqual([])
    // The questions that were asked are the measure, not the gold of the sample.
    expect((await run(daemon(extracted(), two(asked.slice(0, 3))), QUESTIONS.slice(0, 3))).outcome).toBe('passed')
  })
})

describe('ACC-IDEM-1', () => {
  /** A job that has ended as `ended`, and what the route answers when its request comes again. */
  const daemon = (ended: Record<string, unknown>, replay?: object, http?: number) => {
    let polls = 0
    return (call: Call): Response => {
      if (call.route === `/jobs/${JOB}`) return answer((polls += 1) < 3 ? job({ status: 'running', attempt: ended.attempt }) : ended)
      const settled = ended.status === 'succeeded' || ended.status === 'cancelled'
      const again = settled ? ended : job({ status: 'queued', attempt: Number(ended.attempt) + 1 })
      return answer(replay ?? again, http ?? (settled ? 200 : 202))
    }
  }
  const DONE = extracted({ attempt: 2 })
  const run = async (ended: Record<string, unknown> = DONE, replay?: object, http?: number) => {
    const { client, calls } = fakeClient(daemon(ended, replay, http))
    return { result: await checkReplay(client, NOTICE, DOC, ASK, STARTED, QUICK), calls }
  }

  it('waits for the job to end, then sends the same request once more', async () => {
    const { result, calls } = await run()
    expect(result).toMatchObject({ id: 'ACC-IDEM-1', outcome: 'passed', problems: [], requeued: false })
    const poll = `GET /jobs/${JOB}`
    expect(calls.map((c) => `${c.method} ${c.route}`)).toEqual([poll, poll, poll, `POST ${EXTRACTIONS}`])
    expect(calls.at(-1)?.json).toEqual({ revision: 1, schema: { fields: ASK } })
  })

  it('passes when a job that succeeded or was cancelled stays as it was, and one that failed is queued again', async () => {
    expect((await run(job({ status: 'cancelled', attempt: 2 }))).result).toMatchObject({ outcome: 'passed', requeued: false })
    expect((await run(failed('engine_unavailable', { attempt: 2 }))).result).toMatchObject({ outcome: 'passed', requeued: true })
    expect((await run(job({ status: 'interrupted', attempt: 2 }))).result).toMatchObject({ outcome: 'passed', requeued: true })
  })

  it('fails when the request started another job, or the answer is no job', async () => {
    expect((await run(DONE, extracted({ job_id: RERUN, attempt: 2 }))).result.problems).toEqual([`the same request started another job: ${RERUN}, not ${JOB}`])
    expect((await run(DONE, { ...DONE, job_id: undefined })).result.problems).toEqual([NOT_A('DocumentsJob')])
    expect((await run(DONE, { ...DONE, status: 'done' })).result.problems).toEqual([NOT_A('DocumentsJob')])
    // The same id on a job that is not this extraction is not the same job.
    for (const other of [{ kind: 'export' }, { document_id: id('doc', 2) }, { revision: 2 }]) expect((await run(DONE, { ...DONE, ...other })).result.problems).toEqual([NOT_OURS])
  })

  it('fails when the job it waited for is not this extraction, though the replay then agrees with it in everything else', async () => {
    // Cancelled at attempt 3 for another document, and replayed as the same job, cancelled at attempt 3, for this one.
    for (const other of [{ document_id: id('doc', 2) }, { revision: 2 }, { kind: 'export' }]) {
      const ended = job({ status: 'cancelled', attempt: 3, ...other })
      const { result, calls } = await run(ended, job({ status: 'cancelled', attempt: 3, document_id: DOC, revision: 1 }))
      expect(result, JSON.stringify(other)).toMatchObject({ outcome: 'failed', problems: [NOT_OURS] })
      expect(result.requeued).toBeUndefined()
      // Nothing is replayed for a job that is another's.
      expect(calls.some((c) => c.method === 'POST')).toBe(false)
    }
    expect((await run(job({ status: 'cancelled', attempt: 3, document_id: DOC, revision: 1 }))).result).toMatchObject({ outcome: 'passed', requeued: false })
  })

  it('fails when the replay runs a finished job again: the same job is not enough', async () => {
    expect((await run(DONE, job({ status: 'queued', attempt: 3 }), 202)).result.problems).toEqual([
      'the replay of a succeeded job answered 202, not 200',
      'the replay left the succeeded job queued at attempt 3, not succeeded at 2',
      'the replay names another extraction',
    ])
    expect((await run(DONE, extracted({ attempt: 3 }))).result.problems).toEqual(['the replay left the succeeded job succeeded at attempt 3, not succeeded at 2'])
    expect((await run(job({ status: 'cancelled', attempt: 2 }), job({ status: 'queued', attempt: 2 }), 202)).result.problems).toEqual([
      'the replay of a cancelled job answered 202, not 200',
      'the replay left the cancelled job queued at attempt 2, not cancelled at 2',
    ])
    // A status that does not go with the job it comes with is no answer of the route.
    expect((await run(job({ status: 'cancelled', attempt: 2 }), job({ status: 'queued', attempt: 2 }))).result.problems).toEqual(['the extraction route answered 200 with a job that is queued'])
    expect((await run(DONE, DONE, 202)).result.problems).toEqual(['the extraction route answered 202 with a job that is succeeded'])
    expect((await run(DONE, extracted({ attempt: 2, result: { extraction_id: id('ext', 2) } }))).result.problems).toEqual(['the replay names another extraction'])
  })

  it('fails when a failed job is not queued again as its next attempt', async () => {
    const lost = failed('engine_unavailable', { attempt: 2 })
    for (const http of [200, 202]) expect((await run(lost, lost, http)).result.problems).toEqual([`the extraction route answered ${http} with a job that is failed`])
    expect((await run(lost, job({ status: 'queued', attempt: 3 }), 200)).result.problems).toEqual(['the extraction route answered 200 with a job that is queued'])
    expect((await run(lost, job({ status: 'running', attempt: 3 }))).result.problems).toEqual(['the replay left the failed job running at attempt 3, not queued at 3'])
    expect((await run(lost, job({ status: 'queued', attempt: 2 }))).result.problems).toEqual(['the replay left the failed job queued at attempt 2, not queued at 3'])
  })
})

describe('ACC-IDEM-2', () => {
  /** Reruns as the route keeps them: one job per key, and a key holds one request. */
  const daemon = (odd: { firstIs?: string; first?: () => Response; secondIs?: string; second?: () => Response; other?: () => Response; cancel?: (jobId: string) => Response } = {}) => {
    const kept = new Map<string, string>()
    return (call: Call): Response => {
      const cancelled = call.route.match(/^\/jobs\/(\w+)\/cancel$/)?.[1]
      if (cancelled) return odd.cancel?.(cancelled) ?? answer(job({ job_id: cancelled, status: 'cancelled' }))
      const key = call.headers['idempotency-key']!
      const body = JSON.stringify(call.json)
      if (!kept.has(key)) {
        kept.set(key, body)
        return odd.first?.() ?? answer(job({ job_id: odd.firstIs ?? RERUN }), 202)
      }
      if (kept.get(key) !== body) return odd.other?.() ?? refuse(422, 'idempotency_key_reused')
      return odd.second?.() ?? answer(job({ job_id: odd.secondIs ?? RERUN }), 202)
    }
  }
  const THIRD = id('job', 3)
  const run = async (odd?: Parameters<typeof daemon>[0], ask = ASK) => {
    const { client, calls } = fakeClient(daemon(odd))
    return { result: await checkRerunKey(client, NOTICE, DOC, ask, STARTED), calls, routes: calls.map((c) => c.route) }
  }

  it('reruns twice under one key, tries the key with another request, and cancels the rerun', async () => {
    const { result, calls, routes } = await run()
    expect(result).toMatchObject({ id: 'ACC-IDEM-2', outcome: 'passed', problems: [] })
    expect(routes).toEqual([EXTRACTIONS, EXTRACTIONS, EXTRACTIONS, `/jobs/${RERUN}/cancel`])
    const reruns = calls.slice(0, 3)
    expect(reruns.every((c) => (c.json as Body).rerun === true)).toBe(true)
    expect(new Set(reruns.map((c) => c.headers['idempotency-key']))).toHaveLength(1)
    expect(reruns[0]?.headers['idempotency-key']).toMatch(/^[\x21-\x7e]{1,200}$/)
  })

  it('makes the other request one the route takes: the same questions, the last with another type', async () => {
    const fields = async (ask: AskField[]) => ((await run({}, ask)).calls[2]!.json as Body).schema.fields
    expect(await fields(ASK)).toEqual([ASK[0], { ...ASK[1], type: 'text' }])
    expect(await fields([{ key: 'only', description: 'x', type: 'text' }])).toEqual([{ key: 'only', description: 'x', type: 'identifier' }])
    expect(await fields([{ key: 'only', description: 'x' }])).toEqual([{ key: 'only', description: 'x', type: 'text' }])
  })

  it('uses a new key every run', async () => {
    const keys = [await run(), await run()].map((r) => r.calls[0]!.headers['idempotency-key'])
    expect(keys[0]).not.toBe(keys[1])
  })

  it('fails when the rerun is the first job, when the key gives two jobs, or when the key takes another request', async () => {
    const same = await run({ firstIs: JOB, secondIs: JOB })
    expect(same.result.problems).toEqual(['the rerun is the job of the first extraction, not a new one'])
    // And then nothing more is asked under that key, and the job is not cancelled: it is the one that is scored.
    expect(same.routes).toEqual([EXTRACTIONS])
    const two = await run({ secondIs: THIRD })
    expect(two.result.problems).toEqual([`the same rerun key started another job: ${THIRD}, not ${RERUN}`])
    const taken = await run({ other: () => answer(job({ job_id: THIRD }), 202) })
    expect(taken.result.problems).toEqual([`the rerun key was taken with another request, as job ${THIRD}`])
    // Every job the reruns started is cancelled, not only the first.
    for (const routes of [two.routes, taken.routes]) expect(routes.slice(3)).toEqual([`/jobs/${RERUN}/cancel`, `/jobs/${THIRD}/cancel`])
    // But never the job of the first extraction, should a later answer name it.
    const back = await run({ secondIs: JOB })
    expect(back.result.problems).toEqual([`the same rerun key started another job: ${JOB}, not ${RERUN}`])
    expect(back.routes.slice(3)).toEqual([`/jobs/${RERUN}/cancel`])
  })

  it('fails when a new rerun is not a job to be run, or its answer is not what the route gives', async () => {
    const first = async (body: object, http: number) => (await run({ first: () => answer(body, http) })).result.problems
    expect(await first(extracted({ job_id: RERUN }), 200)).toEqual(['a new rerun was answered with a job that is succeeded'])
    expect(await first(job({ job_id: RERUN, status: 'cancelled' }), 200)).toEqual(['a new rerun was answered with a job that is cancelled'])
    expect(await first(job({ job_id: RERUN, status: 'cancelling' }), 202)).toEqual(['a new rerun was answered with a job that is cancelling'])
    expect(await first(job({ job_id: RERUN, status: 'running' }), 202)).toEqual([])
    expect(await first(job({ job_id: RERUN }), 200)).toEqual(['the extraction route answered 200 with a job that is queued'])
    for (const ended of [failed('engine_unavailable', { job_id: RERUN }), job({ job_id: RERUN, status: 'interrupted' })]) {
      expect(await first(ended, 202)).toEqual([`the extraction route answered 202 with a job that is ${ended.status}`])
    }
    // The job of an answer with the wrong status is there all the same, and is cancelled.
    expect((await run({ first: () => answer(job({ job_id: RERUN }), 200) })).routes).toEqual([EXTRACTIONS, `/jobs/${RERUN}/cancel`])
    // One that is not this document's extraction is not ours to cancel.
    expect((await run({ first: () => answer(job({ job_id: RERUN, document_id: id('doc', 2) }), 202) })).routes).toEqual([EXTRACTIONS])
    // Nor is the job of the first extraction, whatever status it comes with.
    expect((await run({ first: () => answer(job(), 200) })).routes).toEqual([EXTRACTIONS])
    const NOT_THIS = [`job ${RERUN} is not an extraction of revision 1 of this document`]
    for (const other of [{ kind: 'export' }, { document_id: id('doc', 2) }, { revision: 2 }]) expect(await first(job({ job_id: RERUN, ...other }), 202)).toEqual(NOT_THIS)
    // The key again: the same job is not enough, when its answer is not one of the route.
    const second = async (body: object, http: number) => run({ second: () => answer(body, http) })
    expect((await second(job({ job_id: RERUN }), 200)).result.problems).toEqual(['the extraction route answered 200 with a job that is queued'])
    expect((await second(job({ job_id: RERUN, kind: 'export' }), 202)).result.problems).toEqual(NOT_THIS)
    expect((await second(extracted({ job_id: RERUN }), 200)).result.problems).toEqual([])
    // What went wrong on the way stops the check, and the rerun is cancelled all the same.
    expect((await second(job({ job_id: RERUN }), 200)).routes).toEqual([EXTRACTIONS, EXTRACTIONS, `/jobs/${RERUN}/cancel`])
  })

  it('keeps what it found before a request failed', async () => {
    const found = await run({ secondIs: THIRD, other: () => refuse(400, 'invalid_request') })
    expect(found.result.problems).toEqual([`the same rerun key started another job: ${THIRD}, not ${RERUN}`, '400 invalid_request: it is invalid_request'])
    expect(found.routes.slice(3)).toEqual([`/jobs/${RERUN}/cancel`, `/jobs/${THIRD}/cancel`])
  })

  it('fails when the other request is refused for another reason than the key, and cancels the rerun all the same', async () => {
    const refused = await run({ other: () => refuse(400, 'invalid_request') })
    expect(refused.result.problems).toEqual(['400 invalid_request: it is invalid_request'])
    expect(refused.routes.at(-1)).toBe(`/jobs/${RERUN}/cancel`)
    expect((await run({ other: () => refuse(422, 'unsupported_format') })).result.problems).toEqual(['422 unsupported_format: it is unsupported_format'])
    expect((await run({ other: () => refuse(409, 'idempotency_key_reused') })).result.problems).toEqual(['409 idempotency_key_reused: it is idempotency_key_reused'])
  })

  it('says so when the rerun could not be cancelled, without losing what it found', async () => {
    const stuck = await run({ secondIs: THIRD, cancel: () => refuse(503, 'storage_unavailable') })
    expect(stuck.result.problems).toEqual([
      `the same rerun key started another job: ${THIRD}, not ${RERUN}`,
      `the rerun ${RERUN} could not be cancelled: 503 storage_unavailable: it is storage_unavailable`,
      `the rerun ${THIRD} could not be cancelled: 503 storage_unavailable: it is storage_unavailable`,
    ])
  })

  it('fails when the cancel leaves the rerun to run, or does not answer as the route does', async () => {
    const after = async (cancelled: object, http = 200) => (await run({ cancel: () => answer(job({ job_id: RERUN, ...cancelled }), http) })).result.problems
    const not = (why: string) => [`the rerun ${RERUN} could not be cancelled: ${why}`]
    expect(await after({ status: 'queued' })).toEqual(not('the job is still queued after its cancel'))
    expect(await after({ status: 'running' })).toEqual(not('the job is still running after its cancel'))
    // Left failed or interrupted, the next request under its key would run it again.
    expect(await after({ status: 'interrupted' })).toEqual(not('the job is still interrupted after its cancel'))
    expect(await after({ status: 'failed', error: { code: 'engine_unavailable', message: 'x' } })).toEqual(not('the job is still failed after its cancel'))
    for (const status of ['cancelling', 'cancelled']) expect(await after({ status }), status).toEqual([])
    expect(await after({ status: 'succeeded', result: { extraction_id: EXT } })).toEqual([])
    expect(await after({ job_id: JOB, status: 'cancelled' })).toEqual(not(`asked to cancel ${RERUN}, answered with ${JOB}`))
    expect(await after({ status: 'cancelled' }, 202)).toEqual(not('the cancel answered 202, not 200'))
    expect(await after({ status: 'cancelled', kind: 'export' })).toEqual(not(`job ${RERUN} is not an extraction of revision 1 of this document`))
    expect(await after({ job_id: undefined })).toEqual([expect.stringContaining(`the rerun ${RERUN} could not be cancelled: the answer is not a DocumentsJob`)])
  })

  it('fails when the rerun is answered with no job', async () => {
    const { client, calls } = fakeClient(() => answer({ status: 'queued' }, 202))
    expect((await checkRerunKey(client, NOTICE, DOC, ASK, STARTED)).problems).toEqual([NOT_A('DocumentsJob')])
    expect(calls).toHaveLength(1)
  })
})

describe('ACC-IDEM-3', () => {
  /** A job as the routes move it: running at attempt 3, then cancelling, then cancelled; a replay leaves it; a retry queues it again. */
  const daemon = (odd: { before?: () => Response; cancel?: object; cancelStatus?: number; ends?: object; replay?: object; replayStatus?: number; retry?: object; retryStatus?: number } = {}) => {
    let polls = 0
    let asked = false
    return (call: Call): Response => {
      if (call.route === `/jobs/${JOB}/cancel`) {
        asked = true
        return answer(odd.cancel ?? job({ status: 'cancelling', attempt: 3 }), odd.cancelStatus ?? 200)
      }
      if (call.route === `/jobs/${JOB}` && !asked) return odd.before?.() ?? answer(job({ status: 'running', attempt: 3 }))
      if (call.route === `/jobs/${JOB}`) return answer((polls += 1) < 3 ? job({ status: 'cancelling', attempt: 3 }) : (odd.ends ?? job({ status: 'cancelled', attempt: 3 })))
      if (call.route === EXTRACTIONS) return answer(odd.replay ?? job({ status: 'cancelled', attempt: 3 }), odd.replayStatus ?? 200)
      if (call.route === `/jobs/${JOB}/retry`) return answer(odd.retry ?? job({ status: 'queued', attempt: 4 }), odd.retryStatus ?? 200)
      return refuse(404, 'not_found')
    }
  }
  const run = async (odd?: Parameters<typeof daemon>[0]) => {
    const { client, calls } = fakeClient(daemon(odd))
    return { result: await checkCancelledStays(client, NOTICE, DOC, ASK, STARTED, QUICK), routes: calls.map((c) => `${c.method} ${c.route}`) }
  }
  const poll = `GET /jobs/${JOB}`

  it('cancels, waits for the job to stop, replays the request, and only then retries', async () => {
    const { result, routes } = await run()
    expect(result).toMatchObject({ id: 'ACC-IDEM-3', sample: NOTICE.id, outcome: 'passed', problems: [], interrupted: true })
    expect(routes).toEqual([poll, `POST /jobs/${JOB}/cancel`, poll, poll, poll, `POST ${EXTRACTIONS}`, `POST /jobs/${JOB}/retry`])
  })

  it('says when the job it cancelled was not running', async () => {
    expect((await run({ cancel: job({ status: 'cancelled', attempt: 3 }) })).result).toMatchObject({ outcome: 'passed', interrupted: false })
  })

  it('is skipped, not failed, only when the cancel found the job already succeeded', async () => {
    const { result, routes } = await run({ cancel: extracted({ attempt: 3 }) })
    expect(result).toMatchObject({ outcome: 'skipped', problems: [] })
    expect(result.interrupted).toBeUndefined()
    expect(routes).toEqual([poll, `POST /jobs/${JOB}/cancel`])
  })

  it('fails when a job that took the cancel ends as anything but cancelled, success included', async () => {
    expect((await run({ ends: extracted({ attempt: 3 }) })).result).toMatchObject({ outcome: 'failed', problems: ['the job was cancelling when cancelled, and then ended succeeded'] })
    expect((await run({ ends: failed('engine_unavailable', { attempt: 3 }) })).result).toMatchObject({ outcome: 'failed', problems: ['the job was cancelling when cancelled, and then ended failed'] })
    expect((await run({ cancel: job({ status: 'cancelled', attempt: 3 }), ends: job({ status: 'interrupted', attempt: 3 }) })).result.problems).toEqual(['the job was cancelled when cancelled, and then ended interrupted'])
  })

  it('fails when the replay revives the job, starts another, changes the attempt, or answers as for a job still to run', async () => {
    const cancelled = (more: object) => job({ status: 'cancelled', attempt: 3, ...more })
    expect((await run({ replay: cancelled({ status: 'queued' }), replayStatus: 202 })).result.problems).toEqual(['the replay made the cancelled job queued'])
    expect((await run({ replay: extracted({ attempt: 3 }) })).result.problems).toEqual(['the replay made the cancelled job succeeded'])
    expect((await run({ replay: cancelled({ job_id: RERUN }) })).result.problems).toEqual([`the replay started another job: ${RERUN}`])
    expect((await run({ replay: cancelled({ attempt: 4 }) })).result.problems).toEqual(['the replay changed the attempt from 3 to 4'])
    expect((await run({ replayStatus: 202 })).result.problems).toEqual(['the extraction route answered 202 with a job that is cancelled'])
    expect((await run({ replay: cancelled({ status: 'cancelling' }), replayStatus: 202 })).result.problems).toEqual(['the replay made the cancelled job cancelling'])
    expect((await run({ replay: cancelled({ status: 'queued' }) })).result.problems).toEqual(['the extraction route answered 200 with a job that is queued'])
    expect((await run({ replay: cancelled({ kind: 'export' }) })).result.problems).toEqual([NOT_OURS])
    expect((await run({ replay: { status: 'cancelled' } })).result.problems).toEqual([NOT_A('DocumentsJob')])
  })

  it('holds the attempt the job was at before the cancel, until the retry: an attempt that moves on the way is not hidden by what came after', async () => {
    // Running at 3, cancelled at 4, replayed at 4, retried to 5: every step agrees with the one before, and the job ran once more than it may.
    const moved = await run({ cancel: job({ status: 'cancelling', attempt: 4 }), ends: job({ status: 'cancelled', attempt: 4 }), replay: job({ status: 'cancelled', attempt: 4 }), retry: job({ status: 'queued', attempt: 5 }) })
    expect(moved.result).toMatchObject({ outcome: 'failed', problems: ['the cancel changed the attempt from 3 to 4'] })
    expect(moved.result.interrupted).toBeUndefined()
    expect(moved.routes).toEqual([poll, `POST /jobs/${JOB}/cancel`])
    // The same, with the cancel finding the job succeeded: that is no skip.
    expect((await run({ cancel: extracted({ attempt: 4 }) })).result).toMatchObject({ outcome: 'failed', problems: ['the cancel changed the attempt from 3 to 4'] })
    // The cancel answers the attempt it found, and the job stops at the next.
    const later = await run({ ends: job({ status: 'cancelled', attempt: 4 }), replay: job({ status: 'cancelled', attempt: 4 }), retry: job({ status: 'queued', attempt: 5 }) })
    expect(later.result.problems).toEqual(['the job was cancelled at attempt 4, not at the 3 it was at', 'the replay changed the attempt from 3 to 4', 'after the retry the job is queued at attempt 5, not queued at 4'])
    // Whatever attempt it was at is the one held to.
    const seventh = { before: () => answer(job({ status: 'running', attempt: 7 })), cancel: job({ status: 'cancelling', attempt: 7 }), ends: job({ status: 'cancelled', attempt: 7 }), replay: job({ status: 'cancelled', attempt: 7 }) }
    expect((await run({ ...seventh, retry: job({ status: 'queued', attempt: 8 }) })).result).toMatchObject({ outcome: 'passed', problems: [] })
    expect((await run(seventh)).result.problems).toEqual(['after the retry the job is queued at attempt 4, not queued at 8'])
  })

  it('fails when the cancel makes of the job what it cannot: a job that had succeeded stays succeeded', async () => {
    // Succeeded, then cancelled by the cancel, replayed as cancelled and retried: every later step agrees, and a result was unwound.
    const before = () => answer(extracted({ attempt: 3 }))
    const unwound = await run({ before, cancel: job({ status: 'cancelled', attempt: 3 }) })
    expect(unwound.result).toMatchObject({ outcome: 'failed', problems: ['the cancel made a job that was succeeded cancelled'] })
    expect(unwound.result.interrupted).toBeUndefined()
    expect(unwound.routes).toEqual([poll, `POST /jobs/${JOB}/cancel`])
    expect((await run({ before, cancel: job({ status: 'cancelling', attempt: 3 }) })).result.problems).toEqual(['the cancel made a job that was succeeded cancelling'])
    // Left succeeded, there was nothing to cancel: not run, as when the job succeeds between the reading and the cancel.
    expect((await run({ before, cancel: extracted({ attempt: 3 }) })).result).toMatchObject({ outcome: 'skipped', problems: [] })
    expect((await run({ cancel: extracted({ attempt: 3 }) })).result).toMatchObject({ outcome: 'skipped', problems: [] })
  })

  it('fails when a job with no worker is not cancelled at once, or comes out of the cancel succeeded', async () => {
    const was = (status: string) => () => answer(status === 'failed' ? failed('engine_unavailable', { attempt: 3 }) : job({ status, attempt: 3 }))
    for (const status of ['failed', 'interrupted', 'cancelled']) {
      expect((await run({ before: was(status), cancel: job({ status: 'cancelling', attempt: 3 }) })).result.problems, status).toEqual([`the cancel made a job that was ${status} cancelling`])
      expect((await run({ before: was(status), cancel: extracted({ attempt: 3 }) })).result, status).toMatchObject({ outcome: 'failed', problems: [`the cancel made a job that was ${status} succeeded`] })
      expect((await run({ before: was(status), cancel: job({ status: 'cancelled', attempt: 3 }) })).result, status).toMatchObject({ outcome: 'passed', interrupted: false })
    }
    // One that was stopping does not come out succeeded either; one that was queued or running may have got anywhere.
    expect((await run({ before: was('cancelling'), cancel: extracted({ attempt: 3 }) })).result.problems).toEqual(['the cancel made a job that was cancelling succeeded'])
    // A job that was being stopped already was not stopped by this cancel.
    expect((await run({ before: was('cancelling') })).result).toMatchObject({ outcome: 'passed', interrupted: false })
    expect((await run({ before: was('cancelling'), cancel: job({ status: 'cancelled', attempt: 3 }) })).result).toMatchObject({ outcome: 'passed', interrupted: false })
    for (const status of ['queued', 'running']) {
      expect((await run({ before: was(status) })).result, status).toMatchObject({ outcome: 'passed', interrupted: true })
      expect((await run({ before: was(status), cancel: job({ status: 'cancelled', attempt: 3 }) })).result, status).toMatchObject({ outcome: 'passed', interrupted: false })
      expect((await run({ before: was(status), cancel: extracted({ attempt: 3 }) })).result.outcome, status).toBe('skipped')
    }
  })

  it('fails, and is not skipped, when the job cannot be read before the cancel, or is not this extraction: nothing is cancelled then', async () => {
    const first = async (before: () => Response) => {
      const { result, routes } = await run({ before })
      expect(routes).toEqual([poll])
      expect(result.outcome).toBe('failed')
      expect(result.interrupted).toBeUndefined()
      return result.problems
    }
    expect(await first(() => refuse(404, 'not_found'))).toEqual(['404 not_found: it is not_found'])
    expect(await first(() => answer({ status: 'running' }))).toEqual([NOT_A('DocumentsJob')])
    expect(await first(() => answer(job({ job_id: RERUN, status: 'running', attempt: 3 })))).toEqual([`asked for job ${JOB}, answered with ${RERUN}`])
    for (const other of [{ kind: 'export' }, { document_id: id('doc', 2) }, { revision: 2 }]) expect(await first(() => answer(job({ status: 'running', attempt: 3, ...other })))).toEqual([NOT_OURS])
  })

  it('fails when the job that stopped is not this extraction, though all else agrees', async () => {
    for (const other of [{ document_id: id('doc', 2) }, { revision: 2 }, { kind: 'export' }]) {
      const { result, routes } = await run({ ends: job({ status: 'cancelled', attempt: 3, ...other }) })
      expect(result, JSON.stringify(other)).toMatchObject({ outcome: 'failed', problems: [NOT_OURS] })
      expect(routes.some((r) => r.startsWith(`POST ${EXTRACTIONS}`) || r.endsWith('/retry'))).toBe(false)
    }
  })

  it('fails when the retry does not queue the job again at the next attempt', async () => {
    expect((await run({ retry: job({ status: 'cancelled', attempt: 4 }) })).result.problems).toEqual(['after the retry the job is cancelled at attempt 4, not queued at 4'])
    expect((await run({ retry: job({ status: 'queued', attempt: 3 }) })).result.problems).toEqual(['after the retry the job is queued at attempt 3, not queued at 4'])
    expect((await run({ retry: job({ status: 'running', attempt: 4 }) })).result.problems).toEqual(['after the retry the job is running at attempt 4, not queued at 4'])
    // The retry is itself the restart: it answers 200, where a replay that queues again answers 202.
    for (const retryStatus of [201, 202]) expect((await run({ retryStatus })).result.problems).toEqual([`the retry answered ${retryStatus}, not 200`])
    for (const other of [{ kind: 'export' }, { document_id: id('doc', 2) }, { revision: 2 }]) {
      expect((await run({ retry: job({ status: 'queued', attempt: 4, ...other }) })).result.problems).toEqual([NOT_OURS])
    }
  })

  it('fails, and is not skipped, when the cancel did not stop the job or did not answer as the route does', async () => {
    for (const status of ['queued', 'running', 'interrupted', 'failed']) {
      const { result, routes } = await run({ cancel: status === 'failed' ? failed('engine_unavailable', { attempt: 3 }) : job({ status, attempt: 3 }) })
      expect(result).toMatchObject({ outcome: 'failed', problems: [`the job is still ${status} after its cancel`] })
      expect(result.interrupted).toBeUndefined()
      expect(routes).toEqual([poll, `POST /jobs/${JOB}/cancel`])
    }
    expect((await run({ cancelStatus: 202 })).result).toMatchObject({ outcome: 'failed', problems: ['the cancel answered 202, not 200'] })
    for (const other of [{ kind: 'export' }, { document_id: id('doc', 2) }, { revision: 2 }]) {
      expect((await run({ cancel: extracted({ attempt: 3, ...other }) })).result).toMatchObject({ outcome: 'failed', problems: [NOT_OURS] })
    }
  })

  it('fails, and is not skipped, when the cancel is refused or answered with no job', async () => {
    const refused = fakeClient(() => refuse(404, 'not_found')).client
    expect(await checkCancelledStays(refused, NOTICE, DOC, ASK, STARTED, QUICK)).toMatchObject({ outcome: 'failed', problems: ['404 not_found: it is not_found'] })
    expect((await run({ cancel: { status: 'succeeded' } })).result).toMatchObject({ outcome: 'failed', problems: [NOT_A('DocumentsJob')] })
  })

  it('fails, and is not skipped, when the cancel or the retry answers with another job', async () => {
    const wrong = [`asked to cancel ${JOB}, answered with ${RERUN}`]
    const succeeded = await run({ cancel: extracted({ job_id: RERUN, attempt: 3 }) })
    expect(succeeded.result).toMatchObject({ outcome: 'failed', problems: wrong })
    expect(succeeded.routes).toEqual([poll, `POST /jobs/${JOB}/cancel`])
    const cancelling = (await run({ cancel: job({ job_id: RERUN, status: 'cancelling', attempt: 3 }) })).result
    expect(cancelling).toMatchObject({ outcome: 'failed', problems: wrong })
    expect(cancelling.interrupted).toBeUndefined()
    expect((await run({ retry: job({ job_id: RERUN, status: 'queued', attempt: 4 }) })).result.problems).toEqual([`asked to retry ${JOB}, answered with ${RERUN}`])
  })
})
