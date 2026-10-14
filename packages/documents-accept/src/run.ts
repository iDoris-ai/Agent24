// One acceptance run (acceptance README §3, §4): every check in its order for
// each sample, then the scoring of what extraction said. Nothing is written
// here; the report is made from what this returns.

import type { components } from '@agent24/api-client'
import { type AnchorScore, fieldAnchors } from './anchors'
import { type CheckResult, type JobWait, checkCapabilities, checkGet, checkImport } from './checks'
import type { Client } from './client'
import { type Extraction, checkCancelledStays, checkExtract, checkReplay, checkRerunKey, startExtraction } from './extract-checks'
import { type FindRead, type TextRead, checkFind, checkRender, checkText, locator } from './read-checks'
import { type AskField, type Sample, isVerified } from './samples'
import { type Extracted, type FieldScore, scoreField } from './score'
import { type Scored, type Totals, coverage, summarize } from './summary'

type Schemas = components['schemas']

export interface SampleRun {
  sample: string
  documentId?: string
  /** Every check of this sample, in the order the notes to §3 give (ACC-IDEM-3
   *  before ACC-EXTRACT, ACC-IDEM-1 and -2 after it); one that could not be run is `blocked`. */
  checks: CheckResult[]
  text?: TextRead
  find?: FindRead
  extraction?: Extraction
  /** ACC-IDEM-1: the replay found a job that had failed and queued it again, rather than a finished one it left as it was (§3). */
  requeued?: boolean
  /** ACC-IDEM-3: the cancel stopped a job that was running, rather than one that was queued or had failed. */
  interrupted?: boolean
  /** Every gold field against what the extraction said for its key. Without
   *  an extraction no field has an answer, and each is scored so (§4.2): a
   *  sample that could not be read counts in every total, as wrong. */
  scored: Scored[]
  coverage: Record<string, 'passed' | 'review' | 'failed'>
}

export interface Run {
  capabilities: CheckResult & { capabilities?: Schemas['DocumentsCapabilities'] }
  /** Empty when the daemon cannot do what the checks need (ACC-CAP). */
  samples: SampleRun[]
  /** Samples left out because their gold is not verified (samples README §5, step 3). */
  unverified: string[]
  /** The samples the idempotency checks were asked to run on: they ran only if these were among the samples. */
  idempotency: { replayOn?: string; cancelOn?: string }
  totals: { answer: Totals; context: Totals }
}

export interface RunOptions extends JobWait {
  /** The sample whose first extraction is replayed and rerun (ACC-IDEM-1, ACC-IDEM-2). */
  replayOn?: string
  /** The sample whose extraction is cancelled while it runs (ACC-IDEM-3). */
  cancelOn?: string
}

const READS = ['ACC-GET', 'ACC-TEXT', 'ACC-RENDER', 'ACC-FIND']
const REPLAYS = ['ACC-IDEM-1', 'ACC-IDEM-2']

/** A check's result without what it read on the way: that has its own place in the run. */
const plain = ({ id, sample, outcome, problems, ms }: CheckResult): CheckResult => ({ id, ...(sample === undefined ? {} : { sample }), outcome, problems, ms })

/** Every gold field of a sample against the values of its extraction (§4.2–§4.4).
 *  A key answered more than once (ACC-EXTRACT fails it) is right only if every
 *  answer is: the worst of them is scored, whatever order they came in. Its
 *  anchors are those of the answer that covers the fewest, of all its answers
 *  that have anchors to score: none of them makes the coverage look better. */
export function scoreSample(sample: Sample, values: Extracted[], find: FindRead | undefined, partial: boolean): Scored[] {
  const locate = locator(find?.located ?? [])
  const rank = (s: FieldScore) => (s.verdict === 'correct' ? 0 : s.verdict === 'review' ? 1 : 2)
  const covered = (a: AnchorScore) => a.anchors.filter((c) => c.by !== undefined).length
  // Fewer covered is worse; as many covered of more gold anchors is worse too.
  const fewer = (a: AnchorScore, b: AnchorScore) => covered(a) < covered(b) || (covered(a) === covered(b) && a.anchors.length > b.anchors.length)
  return sample.gold.fields.map((gold) => {
    const answers: (Extracted | undefined)[] = values.filter((v) => v.key === gold.key)
    const scored = (answers.length > 0 ? answers : [undefined]).map((value) => ({ value, score: scoreField(gold, value, partial) }))
    const { value, score } = scored.reduce((worst, s) => (rank(s.score) > rank(worst.score) ? s : worst))
    const anchors = scored.map((s) => fieldAnchors(gold, s.value, locate)).reduce<AnchorScore | undefined>((worst, a) => (a !== undefined && (worst === undefined || fewer(a, worst)) ? a : worst), undefined)
    return { sample: sample.id, gold, score, ...(value ? { result: value } : {}), ...(anchors ? { anchors } : {}) }
  })
}

/** The checks of one sample, in the order of §3; what they read is left in `run`. */
async function check(client: Client, sample: Sample, ask: AskField[], options: RunOptions, run: SampleRun): Promise<void> {
  const done = (...results: CheckResult[]) => void run.checks.push(...results.map(plain))
  const block = (ids: string[], why: string) => done(...ids.map((id): CheckResult => ({ id, sample: sample.id, outcome: 'blocked', problems: [why], ms: 0 })))
  const cancel = options.cancelOn === sample.id
  const replays = options.replayOn === sample.id ? REPLAYS : []

  const imported = await checkImport(client, sample, options)
  done(imported)
  if (imported.documentId === undefined) return block([...READS, ...(cancel ? ['ACC-IDEM-3'] : []), 'ACC-EXTRACT', ...replays], 'the sample was not imported')
  const documentId = (run.documentId = imported.documentId)
  done(await checkGet(client, sample, documentId))
  const text = await checkText(client, sample, documentId)
  done(text, await checkRender(client, sample, documentId))
  const find = await checkFind(client, sample, documentId)
  done(find)
  run.text = text.text
  run.find = find.find

  const asked = Date.now()
  let job: Schemas['DocumentsJob']
  try {
    job = await startExtraction(client, documentId, ask)
  } catch (error) {
    if (cancel) block(['ACC-IDEM-3'], 'the extraction was not started')
    done({ id: 'ACC-EXTRACT', sample: sample.id, outcome: 'failed', problems: [error instanceof Error ? error.message : String(error)], ms: Date.now() - asked })
    return block(replays, 'the extraction was not started')
  }
  if (cancel) {
    // Cancelled and then retried, the job is queued again: that attempt is the one read below (§3).
    const stays = await checkCancelledStays(client, sample, documentId, ask, job, options)
    done(stays)
    if (stays.interrupted !== undefined) run.interrupted = stays.interrupted
    // A job this check cancelled and did not get queued again has nothing to
    // read, now or after any wait: that is no failure of the extraction.
    const left = stays.outcome === 'failed' ? await client.json<Schemas['DocumentsJob']>('GET', `/jobs/${job.job_id}`, 'DocumentsJob').then((j) => j.status, () => undefined) : undefined
    if (left === 'cancelled' || left === 'cancelling') return block(['ACC-EXTRACT', ...replays], `ACC-IDEM-3 left the job ${left}`)
  }
  const extracted = await checkExtract(client, sample, documentId, ask, job, options)
  done(extracted)
  run.extraction = extracted.extraction
  // Only the job of an extraction that was read is replayed: one that failed
  // would be queued again by the replay (§5.4), and nobody would read it.
  if (replays.length === 0) return
  if (run.extraction === undefined) return block(replays, 'no extraction was read to replay')
  const replayed = await checkReplay(client, sample, documentId, ask, job, options)
  // The checks run one after another, never two on this document at once:
  // ACC-IDEM-3 counts on no other cancel or retry of its job (AFTER_CANCEL).
  done(replayed, await checkRerunKey(client, sample, documentId, ask, job))
  if (replayed.requeued !== undefined) run.requeued = replayed.requeued
}

async function runSample(client: Client, sample: Sample, ask: AskField[], options: RunOptions): Promise<SampleRun> {
  const run: SampleRun = { sample: sample.id, checks: [], scored: [], coverage: {} }
  await check(client, sample, ask, options, run)
  run.scored = scoreSample(sample, run.extraction?.values ?? [], run.find, run.text?.partial ?? false)
  run.coverage = coverage(sample.id, sample.meta.covers, run.scored)
  return run
}

/** Runs the checks on these samples, one after the other. When the daemon
 *  cannot do what they need, nothing more is asked of it (ACC-CAP, §3). */
export async function run(client: Client, samples: { sample: Sample; ask: AskField[] }[], options: RunOptions = {}): Promise<Run> {
  const capabilities = await checkCapabilities(client)
  const verified = samples.filter((s) => isVerified(s.sample.gold))
  const unverified = samples.filter((s) => !isVerified(s.sample.gold)).map((s) => s.sample.id)
  const done: SampleRun[] = []
  if (capabilities.outcome === 'passed') for (const { sample, ask } of verified) done.push(await runSample(client, sample, ask, options))
  const idempotency = { ...(options.replayOn === undefined ? {} : { replayOn: options.replayOn }), ...(options.cancelOn === undefined ? {} : { cancelOn: options.cancelOn }) }
  return { capabilities, samples: done, unverified, idempotency, totals: summarize(done.flatMap((s) => s.scored)) }
}
