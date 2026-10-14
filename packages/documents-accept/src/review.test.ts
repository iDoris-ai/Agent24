import { describe, expect, it } from 'vitest'
import { type Ran, type Report, buildReport } from './report'
import { type ReviewItem, applyReview, readReport, readReview } from './review'
import { run } from './run'
import { loadAsk } from './samples'
import { HOLIDAYS, NOTICE, type Odd, daemon } from './test/daemon'
import { anchor, answer, failed, fakeClient, id, refuse } from './test/fake'

const RAN: Ran = { commit: 'abc1234', machine: { model: 'Mac16,10', os: 'macOS 15.2' }, at: new Date('2026-10-14T08:00:00Z'), secrets: [] }
const EXT = id('ext')
const here = (block: string) => anchor(1, block, 0, 4, { content_sha256: `sha256:${NOTICE.meta.sha256_stored}` })
/** What the extraction says: one value for a person to settle, one right, one wrong, one right without a source; the rest it does not find. */
const SAID: Record<string, object> = {
  lift_in_service: { status: 'present', value: '照常运行', anchors: [here('p1/b9')] },
  work_order: { status: 'present', value: 'WO-000731', anchors: [here('p1/b1')] },
  store_water_in_advance: { status: 'present', value: '不用储水', anchors: [here('p1/b1')] },
  contact_party: { status: 'present', value: '物业服务中心', anchors: [], unsourced_reason: 'no span matched' },
}
const values = loadAsk(NOTICE).map((f) => ({ key: f.key, ...(SAID[f.key] ?? { status: 'missing', missing_reason: 'not_in_document' }) }))
const report = async (odd: Odd = { values }): Promise<Report> => {
  const samples = [NOTICE, HOLIDAYS].map((sample) => ({ sample, ask: loadAsk(sample) }))
  return buildReport(await run(fakeClient(daemon(odd)).client, samples.slice(0, 1), { sleep: async () => {} }), [NOTICE, HOLIDAYS], RAN)
}
const on = (more: Partial<ReviewItem>): ReviewItem => ({ sample: NOTICE.id, extraction_id: EXT, verdict: 'correct', ...more })
const review = (items: ReviewItem[]) => ({ reviewer: 'david', items })
const field = (r: Report, key: string) => r.samples[0]!.fields.find((f) => f.key === key)!
/** How many marks of a review a report holds: the review itself, and one on each field, anchor or must_not_assert decided. */
const reviews = (r: Report): number =>
  ('review' in r ? 1 : 0) + r.samples.flatMap((s) => [...s.fields, ...s.fields.flatMap((f) => f.anchors?.anchors ?? []), ...s.must_not_assert]).filter((m) => 'review' in m).length

describe('applyReview', () => {
  it('sets what a person decided beside the mechanical verdict, and counts the totals once more', async () => {
    const before = await report()
    expect(field(before, 'lift_in_service').score.verdict).toBe('review')
    expect(before.totals.answer).toMatchObject({ review: 1, criticalReview: [{ sample: NOTICE.id, key: 'lift_in_service' }] })
    const after = applyReview(before, review([on({ field: 'lift_in_service', note: '说的是 1 号电梯' })]))
    expect(field(after, 'lift_in_service')).toMatchObject({ score: { verdict: 'review' }, review: { verdict: 'correct', reviewer: 'david', note: '说的是 1 号电梯' } })
    expect(after.review).toMatchObject({ reviewer: 'david', applied: 1, unused: [] })
    expect(after.review.totals.answer).toMatchObject({ correct: before.totals.answer.correct + 1, review: 0, criticalReview: [] })
    // The mechanical totals stay as they were, and the report that came in is not written on.
    expect(after.totals).toEqual(before.totals)
    expect(field(before, 'lift_in_service').review).toBeUndefined()
    expect(after.review.totals.context).toEqual(before.totals.context)
  })

  it('lets a person overturn any mechanical verdict, either way', async () => {
    const before = await report()
    const critical = (r: { answer: { critical: { key: string }[] } }) => r.answer.critical.map((c) => c.key)
    expect(critical(before.totals)).toContain('shutoff_start_time')
    const kinder = applyReview(before, review([on({ field: 'shutoff_start_time' })]))
    expect(critical(kinder.review.totals)).not.toContain('shutoff_start_time')
    expect(kinder.review.totals.answer.correct).toBe(before.totals.answer.correct + 1)
    expect(field(before, 'contact_phone').score.verdict).toBe('correct')
    const harsher = applyReview(before, review([on({ field: 'contact_phone', verdict: 'fabricated' }), on({ field: 'lift_in_service', verdict: 'mismatch' })]))
    expect(harsher.review.totals.answer.correct).toBe(before.totals.answer.correct - 1)
    expect(harsher.review.totals.answer.missing.correct).toBe(before.totals.answer.missing.correct - 1)
    expect(harsher.review.totals.answer.critical).toEqual(expect.arrayContaining([{ sample: NOTICE.id, key: 'contact_phone', verdict: 'fabricated' }, { sample: NOTICE.id, key: 'lift_in_service', verdict: 'mismatch' }]))
    expect(harsher.review.totals.answer.review).toBe(0)
  })

  it('lets a person say of one gold anchor that it is covered, or that it is not', async () => {
    const before = await report()
    const lift = field(before, 'lift_in_service').anchors!
    expect(lift.anchors.map((a) => a.by)).toEqual([undefined, undefined])
    const covered = applyReview(before, review([on({ field: 'lift_in_service', anchor: 2, verdict: 'covered' })]))
    expect(field(covered, 'lift_in_service').anchors!.anchors.map((a) => a.review?.verdict)).toEqual([undefined, 'covered'])
    expect(field(covered, 'lift_in_service').anchors!.anchors[1]!.by).toBeUndefined()
    expect(covered.review.totals.answer.anchors).toEqual({ ...before.totals.answer.anchors, covered: before.totals.answer.anchors.covered + 1 })
    expect(field(covered, 'lift_in_service').review).toBeUndefined()
    // The other way, on an anchor the machine had covered (a context field's).
    expect(field(before, 'work_order').anchors!.anchors[0]!.by).toBe('place')
    const uncovered = applyReview(before, review([on({ field: 'work_order', anchor: 1, verdict: 'uncovered' })]))
    expect(uncovered.review.totals.context.anchors.covered).toBe(before.totals.context.anchors.covered - 1)
    expect(uncovered.review.totals.answer.anchors).toEqual(before.totals.answer.anchors)
    expect(field(uncovered, 'work_order').anchors!.anchors[0]).toMatchObject({ by: 'place', review: { verdict: 'uncovered', reviewer: 'david' } })
  })

  it('records what a person found of a must_not_assert, and counts the violations and what is still to judge', async () => {
    const before = await report()
    const all = NOTICE.gold.must_not_assert.length
    expect(applyReview(before, review([])).review.must_not_assert).toEqual({ violated: 0, unreviewed: all })
    const after = applyReview(before, review([on({ must_not_assert: 3, verdict: 'not_violated' }), on({ must_not_assert: 6, verdict: 'violated', note: '把两处的时刻拼在一起' }), on({ must_not_assert: all, verdict: 'violated' })]))
    expect(after.review.must_not_assert).toEqual({ violated: 2, unreviewed: all - 3 })
    const said = after.samples[0]!.must_not_assert
    expect(said[2]).toMatchObject({ n: 3, verdict: 'unreviewed', review: { verdict: 'not_violated', reviewer: 'david' } })
    expect(said[5]!.review).toEqual({ verdict: 'violated', reviewer: 'david', note: '把两处的时刻拼在一起' })
    expect(said.filter((m) => m.review).map((m) => m.n)).toEqual([3, 6, all])
    expect(after.review.totals).toEqual(before.totals)
  })

  it('merged again, starts from the mechanical verdicts: what was decided before and is not decided now is gone', async () => {
    const before = await report()
    const first = applyReview(before, review([on({ field: 'lift_in_service' }), on({ field: 'work_order', anchor: 1, verdict: 'uncovered' }), on({ must_not_assert: 3, verdict: 'violated' })]))
    const second = applyReview(first, { reviewer: 'jason', items: [on({ field: 'shutoff_start_time' })] })
    expect(second.samples).toEqual(applyReview(before, { reviewer: 'jason', items: [on({ field: 'shutoff_start_time' })] }).samples)
    expect(second.review).toMatchObject({ reviewer: 'jason', applied: 1, must_not_assert: { violated: 0, unreviewed: NOTICE.gold.must_not_assert.length } })
    expect(field(second, 'lift_in_service').review).toBeUndefined()
    expect(field(second, 'work_order').anchors!.anchors[0]!.review).toBeUndefined()
    expect(second.samples[0]!.must_not_assert[2]!.review).toBeUndefined()
    expect(applyReview(first, review([])).samples).toEqual(before.samples)
    // The same review merged twice is the same report.
    expect(applyReview(first, review([on({ field: 'lift_in_service' })]))).toEqual(applyReview(before, review([on({ field: 'lift_in_service' })])))
  })

  it('settles the coverage points again by what a person decided, beside the mechanical ones', async () => {
    const before = await report()
    expect(before.samples[0]!.coverage).toMatchObject({ C9: 'passed', C6: 'failed', C7: 'failed' })
    const nothing = applyReview(before, review([]))
    expect(nothing.review.coverage).toEqual({ [NOTICE.id]: before.samples[0]!.coverage })
    // C9 stands on contact_phone alone; C6 on shutoff_registration_not_needed; C7 on two fields.
    const after = applyReview(before, review([on({ field: 'contact_phone', verdict: 'fabricated' }), on({ field: 'shutoff_registration_not_needed' }), on({ field: 'affected_units' })]))
    expect(after.review.coverage[NOTICE.id]).toEqual({ ...before.samples[0]!.coverage, C9: 'failed', C6: 'passed' })
    expect(applyReview(before, review([on({ field: 'affected_units' }), on({ field: 'drinking_water_limit' })])).review.coverage[NOTICE.id]).toMatchObject({ C7: 'passed' })
    expect(after.samples[0]!.coverage).toEqual(before.samples[0]!.coverage)
  })

  it('settles the coverage of each sample by its own fields, though two samples have a field of one name', async () => {
    const samples = [NOTICE, HOLIDAYS].map((sample) => ({ sample, ask: loadAsk(sample) }))
    const two = buildReport(await run(fakeClient(daemon()).client, samples, { sleep: async () => {} }), [NOTICE, HOLIDAYS], RAN)
    // The same report, with the second sample standing a point on a field named as one of the first's.
    const twin: Report = { ...two, samples: [two.samples[0]!, { ...two.samples[1]!, covers: { C9: ['contact_phone'] }, coverage: { C9: 'failed' }, fields: [{ ...two.samples[0]!.fields.find((f) => f.key === 'contact_phone')!, score: { key: 'contact_phone', verdict: 'fabricated' } }] }] }
    const kinder = applyReview(twin, review([{ sample: HOLIDAYS.id, extraction_id: EXT, field: 'contact_phone', verdict: 'correct' }]))
    expect(kinder.review.coverage[HOLIDAYS.id]).toEqual({ C9: 'passed' })
    expect(kinder.review.coverage[NOTICE.id]).toEqual(two.samples[0]!.coverage)
    const harsher = applyReview(twin, review([on({ field: 'contact_phone', verdict: 'fabricated' })]))
    expect(harsher.review.coverage).toMatchObject({ [NOTICE.id]: { C9: 'failed' }, [HOLIDAYS.id]: { C9: 'failed' } })
    expect(applyReview(twin, review([])).review.coverage).toMatchObject({ [NOTICE.id]: { C9: 'passed' }, [HOLIDAYS.id]: { C9: 'failed' } })
  })

  it('counts an anchor a person found covered as covered, and no longer as one find could not place', async () => {
    const lost = await report({ values, find: () => answer({ parse_status: 'complete', matches: [], next_cursor: null }) })
    const { anchors } = lost.totals.answer
    expect(anchors.unlocated).toBeGreaterThan(1)
    expect(field(lost, 'lift_in_service').anchors!.anchors.map((a) => [a.by, a.unlocated])).toEqual([[undefined, true], [undefined, true]])
    const after = applyReview(lost, review([on({ field: 'lift_in_service', anchor: 1, verdict: 'covered' }), on({ field: 'lift_in_service', anchor: 2, verdict: 'uncovered' })]))
    expect(after.review.totals.answer.anchors).toEqual({ gold: anchors.gold, covered: anchors.covered + 1, unlocated: anchors.unlocated - 1 })
  })

  it('with nothing decided, counts the same totals', async () => {
    const before = await report()
    expect(applyReview(before, review([])).review).toMatchObject({ applied: 0, unused: [], totals: before.totals })
  })
})

describe('applyReview, with a decision that fits nothing', () => {
  const unused = async (item: ReviewItem, odd?: Odd) => {
    const before = await report(odd)
    const after = applyReview(before, review([item]))
    expect(after.review).toMatchObject({ applied: 0, totals: before.totals })
    expect(after.samples).toEqual(before.samples)
    expect(after.review.unused).toEqual([{ item, why: expect.any(String) }])
    return after.review.unused[0]!.why
  }

  it('leaves out a decision about another extraction: it was made of other values', async () => {
    expect(await unused(on({ field: 'lift_in_service', extraction_id: id('ext', 2) }))).toBe(`it is about extraction ${id('ext', 2)}, and the report is of ${EXT}`)
    expect(await unused(on({ must_not_assert: 1, verdict: 'violated', extraction_id: id('ext', 2) }))).toBe(`it is about extraction ${id('ext', 2)}, and the report is of ${EXT}`)
    expect(await unused(on({ field: 'lift_in_service' }), { ends: failed('engine_unavailable') })).toBe(`it is about extraction ${EXT}, and the report is of none`)
    expect(await unused(on({ field: 'lift_in_service', sample: HOLIDAYS.id }))).toBe(`the report has no sample ${HOLIDAYS.id}`)
  })

  it('leaves out a decision that names nothing the sample has', async () => {
    expect(await unused(on({ field: 'no_such_field' }))).toBe('the sample has no field no_such_field')
    expect(await unused(on({ must_not_assert: 99, verdict: 'violated' }))).toBe('the sample has no must_not_assert 99')
    expect(await unused(on({ must_not_assert: 0, verdict: 'violated' }))).toBe('the sample has no must_not_assert 0')
    expect(await unused(on({ field: 'lift_in_service', anchor: 3, verdict: 'covered' }))).toBe('the field lift_in_service has no scored anchor 3')
    expect(await unused(on({ field: 'lift_in_service', anchor: 0, verdict: 'covered' }))).toBe('the field lift_in_service has no scored anchor 0')
    // A field the extraction did not find has no anchors that were scored.
    expect(await unused(on({ field: 'shutoff_start_time', anchor: 1, verdict: 'covered' }))).toBe('the field shutoff_start_time has no scored anchor 1')
    for (const item of [on({}), on({ anchor: 1 }), on({ field: 'lift_in_service', must_not_assert: 1 }), on({ must_not_assert: 1, anchor: 1, verdict: 'violated' })]) {
      expect(await unused(item)).toBe('it names neither one field nor one must_not_assert')
    }
  })

  it('leaves out a verdict that is not one of its kind', async () => {
    for (const verdict of ['review', 'covered', 'violated', 'ok', '']) expect(await unused(on({ field: 'lift_in_service', verdict }))).toBe(`${verdict} is no verdict on a field the gold has as present`)
    // Nor a verdict §4.2 has for a field of another kind: nothing can be made up of a field that is there, or chosen where there is no conflict.
    const no = async (key: string, status: string, verdicts: string[]) => {
      for (const verdict of verdicts) expect(await unused(on({ field: key, verdict })), `${key} ${verdict}`).toBe(`${verdict} is no verdict on a field the gold has as ${status}`)
    }
    await no('lift_in_service', 'present', ['fabricated', 'chose_one', 'unanswered'])
    await no('contact_phone', 'missing', ['mismatch', 'missed', 'chose_one', 'false_conflict'])
    await no('shutoff_date', 'conflict', ['fabricated', 'false_conflict', 'unanswered'])
    const before = await report()
    const taken = [...['correct', 'mismatch', 'missed', 'false_conflict'].map((verdict) => on({ field: 'lift_in_service', verdict })), ...['correct', 'fabricated', 'unanswered'].map((verdict) => on({ field: 'contact_phone', verdict })), ...['correct', 'mismatch', 'missed', 'chose_one'].map((verdict) => on({ field: 'shutoff_date', verdict }))]
    for (const item of taken) expect(applyReview(before, review([item])).review, `${item.field} ${item.verdict}`).toMatchObject({ applied: 1, unused: [] })
    for (const verdict of ['correct', 'violated', 'all']) expect(await unused(on({ field: 'lift_in_service', anchor: 1, verdict }))).toBe(`${verdict} is no verdict on an anchor`)
    for (const verdict of ['correct', 'covered', 'unreviewed']) expect(await unused(on({ must_not_assert: 1, verdict }))).toBe(`${verdict} is no verdict on a must_not_assert`)
  })

  it('takes the first of two decisions on one thing, and lists the second', async () => {
    const before = await report()
    const twice = [on({ field: 'lift_in_service', verdict: 'mismatch' }), on({ field: 'lift_in_service' }), on({ field: 'lift_in_service', anchor: 1, verdict: 'covered' }), on({ must_not_assert: 1, verdict: 'violated' }), on({ must_not_assert: 1, verdict: 'not_violated' })]
    const after = applyReview(before, review(twice))
    expect(after.review).toMatchObject({ applied: 3, unused: [{ item: twice[1], why: 'it is decided twice' }, { item: twice[4], why: 'it is decided twice' }] })
    expect(field(after, 'lift_in_service').review?.verdict).toBe('mismatch')
    expect(after.review.must_not_assert.violated).toBe(1)
    // Also when the first was no decision that could be taken: the reviewer has to say which is meant.
    const unclear = [on({ field: 'lift_in_service', verdict: 'review' }), on({ field: 'lift_in_service', verdict: 'mismatch' })]
    expect(applyReview(before, review(unclear)).review).toMatchObject({ applied: 0, unused: [{ item: unclear[0] }, { item: unclear[1], why: 'it is decided twice' }] })
    // A decision about another extraction is about another thing: it does not use up the place.
    const stale = [on({ field: 'lift_in_service', extraction_id: id('ext', 2) }), on({ field: 'lift_in_service', sample: 'no-such-sample' }), on({ field: 'lift_in_service' })]
    expect(applyReview(before, review(stale)).review).toMatchObject({ applied: 1, unused: [{ item: stale[0] }, { item: stale[1] }] })
  })
})

describe('readReview', () => {
  it('takes a review as §4.7 writes it', () => {
    const written = { reviewer: 'david', items: [on({ field: 'lift_in_service', note: '' }), on({ must_not_assert: 3, verdict: 'not_violated' }), on({ field: 'work_order', anchor: 1, verdict: 'covered' })] }
    expect(readReview(written)).toEqual(written)
    expect(readReview({ reviewer: 'david', items: [] }).items).toEqual([])
  })

  it('refuses what is not a review, and says which item is not a decision', () => {
    for (const bad of [null, [], 'david', {}, { reviewer: 'david' }, { items: [] }, { reviewer: ' ', items: [] }, { reviewer: 7, items: [] }, { reviewer: 'david', items: {} }]) {
      expect(() => readReview(bad)).toThrow('review.json must have a reviewer and items')
    }
    const second = (item: unknown) => () => readReview({ reviewer: 'david', items: [on({ field: 'work_order' }), item] })
    expect(second({ ...on({ field: 'work_order' }), bearer: 'tok', n: 1 })).toThrow('review.json: item 2 has what a decision does not have: bearer, n')
    expect(() => readReview({ reviewer: 'david', items: [], token: 'tok' })).toThrow('review.json must have a reviewer and items, and nothing else')
    for (const bad of [null, 'x', on({ sample: '' }), on({ extraction_id: undefined as never }), on({ verdict: 7 as never }), on({ field: 7 as never }), on({ note: 7 as never }), on({ anchor: 1.5 }), on({ anchor: '1' as never }), on({ must_not_assert: -1 })]) {
      expect(second(bad)).toThrow('review.json: item 2 is not a decision')
    }
  })
})

describe('readReport', () => {
  it('takes a report as the harness wrote it, and of a reviewed one leaves the review out', async () => {
    const written = JSON.parse(JSON.stringify(await report())) as Report
    expect(readReport(written)).toEqual(written)
    const decided = review([on({ field: 'lift_in_service' }), on({ field: 'work_order', anchor: 1, verdict: 'uncovered' }), on({ must_not_assert: 1, verdict: 'violated' })])
    const reviewed = JSON.parse(JSON.stringify(applyReview(written, decided))) as Report
    expect(reviews(reviewed)).toBe(4)
    expect(reviews(readReport(reviewed))).toBe(0)
    expect(readReport(reviewed)).toEqual(written)
    // What a person decided is merged again, to the same numbers.
    expect(JSON.parse(JSON.stringify(applyReview(readReport(reviewed), decided)))).toEqual(reviewed)
    expect(readReport(JSON.parse(JSON.stringify(await report({ ends: failed('engine_unavailable') }))))).toMatchObject({ samples: [{ sample: NOTICE.id }] })
    // One of a run that ran nothing is a report all the same.
    const unmet = buildReport(await run(fakeClient(daemon({ capabilities: () => refuse(404, 'not_found') })).client, [], { sleep: async () => {} }), [], RAN)
    expect(readReport(JSON.parse(JSON.stringify(unmet))).samples).toEqual([])
  })

  it('refuses what is no report of this harness', async () => {
    const written = JSON.parse(JSON.stringify(await report())) as Report
    const NOT = 'report.json is not a report of this harness (documents-accept-report-v1)'
    for (const bad of [null, 'x', [], {}, { schema: 'documents-accept-report-v1' }, { ...written, schema: 'something-else-v9' }, { ...written, samples: {} }, { ...written, samples: [null] }, { ...written, samples: [{ ...written.samples[0], fields: undefined }] }, { ...written, samples: [{ ...written.samples[0], must_not_assert: undefined }] }, { ...written, samples: [{ ...written.samples[0], covers: undefined }] }, { ...written, samples: [{ ...written.samples[0], covers: null }] }, { ...written, samples: [{ ...written.samples[0], covers: { C9: null } }] }, { ...written, samples: [{ ...written.samples[0], covers: { C9: [1] } }] }, { ...written, samples: [{ ...written.samples[0], fields: [null] }] }, { ...written, samples: [{ ...written.samples[0], fields: [{ key: 'x' }] }] }]) {
      expect(() => readReport(bad)).toThrow(NOT)
    }
  })

  it('refuses a report whose totals or coverage points no longer follow from its fields', async () => {
    const written = JSON.parse(JSON.stringify(await report())) as Report
    const CHANGED = 'report.json: its totals or coverage points do not follow from its fields; it was changed after it was written'
    const answers = written.totals.answer
    expect(() => readReport({ ...written, totals: { ...written.totals, answer: { ...answers, correct: answers.fields } } })).toThrow(CHANGED)
    expect(() => readReport({ ...written, totals: { ...written.totals, answer: { ...answers, critical: [] } } })).toThrow(CHANGED)
    const kinder = structuredClone(written)
    kinder.samples[0]!.fields.find((f) => f.key === 'shutoff_start_time')!.score.verdict = 'correct'
    expect(() => readReport(kinder)).toThrow(CHANGED)
    const covered = structuredClone(written)
    covered.samples[0]!.fields.find((f) => f.key === 'lift_in_service')!.anchors!.anchors[0]!.by = 'place'
    expect(() => readReport(covered)).toThrow(CHANGED)
    const passed = structuredClone(written)
    const point = Object.keys(passed.samples[0]!.coverage).find((p) => passed.samples[0]!.coverage[p] !== 'passed')!
    passed.samples[0]!.coverage[point] = 'passed'
    expect(() => readReport(passed)).toThrow(CHANGED)
  })

  it('says a report is none of this harness only when a part of it is of no shape it writes; any other error is said as it is', async () => {
    const written = JSON.parse(JSON.stringify(await report())) as Report
    // The error comes from a getter, not from a part of no shape: this pins which errors are converted, not what raises one.
    const failing = (error: Error) => Object.defineProperty({ ...written }, 'totals', { enumerable: true, get: () => { throw error } })
    const thrown = (error: Error): unknown => {
      try {
        readReport(failing(error))
      } catch (caught) {
        return caught
      }
      return 'nothing was thrown'
    }
    const odd = new TypeError('a part of no shape')
    const converted = thrown(odd)
    expect(converted).toBeInstanceOf(Error)
    expect((converted as Error).message).toBe('report.json is not a report of this harness (documents-accept-report-v1)')
    expect((converted as Error).cause).toBe(odd)
    const other = new RangeError('something else went wrong')
    expect(thrown(other)).toBe(other)
  })

  it('reads none of what a review merged in, whatever was made of it', async () => {
    const reviewed = JSON.parse(JSON.stringify(applyReview(await report(), review([on({ field: 'lift_in_service' })])))) as Report & { review: { totals: { answer: { correct: number; critical: unknown[] } } } }
    const kinder = structuredClone(reviewed)
    kinder.review.totals.answer.correct = 999
    kinder.review.totals.answer.critical = []
    const relabelled = structuredClone(reviewed)
    field(relabelled, 'lift_in_service').review!.verdict = 'fabricated'
    const garbage = { ...structuredClone(reviewed), review: 'garbage' }
    const nonsense = structuredClone(reviewed)
    field(nonsense, 'work_order').review = { verdict: 'nonsense' as never, reviewer: 'x' }
    for (const edited of [kinder, relabelled, garbage, nonsense]) expect(reviews(readReport(edited))).toBe(0)
  })
})
