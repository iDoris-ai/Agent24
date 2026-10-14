// The evidence of one run (acceptance README §5): what `report.json` holds.
// It is made from what the run returned and from the samples' own files, and
// says nothing of the machine but what §5 asks for.

import crypto from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import type { AnchorCover, AnchorScore } from './anchors'
import type { CheckResult } from './checks'
import type { TextRead } from './read-checks'
import type { Run, SampleRun } from './run'
import type { GoldField, Sample } from './samples'
import type { Extracted, FieldScore, Verdict } from './score'
import type { Totals } from './summary'

/** What only the one who ran it knows of a run. */
export interface Ran {
  /** The commit the harness and the daemon were built from. */
  commit: string
  machine: { model: string; os: string }
  at: Date
  /** Text that must not be in a report: the token, the host's name, the user's home. */
  secrets: string[]
}

type Outcome = CheckResult['outcome']

/** One row of the trace chain (README, Appendix A). */
export interface TraceRow {
  scenario: string
  contract: string
  check: string
  owner: string
  phase: string
  /** `not_run`: no sample was run through this check. */
  conclusion: Outcome | 'not_run'
  results: Partial<Record<Outcome, number>>
}

/** What a person decided in review, beside the mechanical verdict it is about (§4.7). */
export interface Decided<V extends string> {
  verdict: V
  reviewer: string
  note?: string
}

export interface FieldReport {
  key: string
  role: 'answer' | 'context'
  severity: GoldField['severity']
  gold: Pick<GoldField, 'status' | 'value' | 'normalized' | 'missing_reason'> & { candidates?: { value: string; normalized: string }[] }
  /** What the extraction said for this key, as it said it; absent when it said nothing. */
  result?: Extracted
  score: FieldScore
  anchors?: Omit<AnchorScore, 'anchors'> & { anchors: (AnchorCover & { review?: Decided<'covered' | 'uncovered'> })[] }
  review?: Decided<Verdict>
}

export interface SampleReport {
  sample: string
  content_sha256: string
  gold_sha256: string
  ask_sha256: string
  document_id?: string
  extraction_id?: string
  model_id?: string
  extractor_version?: string
  checks: CheckResult[]
  /** ACC-IDEM-1, where it ran: whether the replay queued a failed job again, or left a finished one as it was (§3). */
  requeued?: boolean
  /** ACC-IDEM-3, where it ran: whether the cancel stopped a job that was running. */
  interrupted?: boolean
  text?: TextRead
  /** Gold anchors `find` reached on their page, and the quotes it did not
   *  reach, each once though several anchors may share it (§4.4). Absent when
   *  ACC-FIND did not pass. `unparsed`: the regions the first page of that
   *  `find` answer said were not searched: of the document, not only of the
   *  anchor's page. */
  find?: { anchors: number; reachable: number; unreached: { page: number; quote: string; refused?: string; unparsed?: unknown[] }[] }
  /** Every gold field. With no `extraction_id` nothing was read, and every field is without an answer. */
  fields: FieldReport[]
  /** The coverage points of the sample (samples README §2): the fields each stands on, and how it came out. */
  covers: Record<string, string[]>
  coverage: SampleRun['coverage']
  /** Every proposition the gold forbids, numbered from 1: for a person to judge (§4.6). */
  must_not_assert: { n: number; proposition: string; unless?: string; verdict: 'unreviewed'; review?: Decided<'violated' | 'not_violated'> }[]
}

export interface Report {
  schema: 'documents-accept-report-v1'
  generated_at: string
  environment: {
    commit: string
    machine: Ran['machine']
    os_version?: string
    engines: { id: string; kind: string; state: string; version: string | null }[]
    /** Every model and extractor that answered in this run: more than one of either is two runs in one. */
    model_ids: string[]
    extractor_versions: string[]
  }
  capabilities: CheckResult
  samples: SampleReport[]
  unverified: string[]
  /** The samples the idempotency checks were asked to run on; one that is not among `samples` was not run. */
  idempotency: Run['idempotency']
  totals: { answer: Totals; context: Totals }
  /** Every gold anchor of the samples that ran, and those `find` reached. The
   *  anchors of the `unread` samples, where ACC-FIND did not pass, count as
   *  not reached (§4.4: the denominator is not made smaller). */
  find: { anchors: number; reachable: number; unread: number }
  trace: TraceRow[]
}

/** §3: what each check is the evidence for. */
const TRACE: [check: string, scenario: string, contract: string][] = [
  ['ACC-CAP', 'A2 A3 A5', 'ADR-DOC-02 §8'],
  ['ACC-IMPORT', 'A1', 'ADR-DOC-02 §4, §5.6'],
  ['ACC-GET', 'A1 A10', 'ADR-DOC-02 §3, §4'],
  ['ACC-TEXT', 'A2 A3', 'ADR-DOC-02 §3.1'],
  ['ACC-RENDER', 'A2', 'ADR-DOC-02 §4'],
  ['ACC-FIND', 'A4, README §11.4', 'ADR-DOC-02 §3.1 (Q14)'],
  ['ACC-EXTRACT', 'A5, C1–C10, README §11.1/4/7/8', 'ADR-DOC-02 §3.2'],
  ['ACC-IDEM-1', 'README §14', 'ADR-DOC-02 §5.7 (1)'],
  ['ACC-IDEM-2', 'README §14', 'ADR-DOC-02 §5.7 (2)'],
  ['ACC-IDEM-3', 'README §14', 'ADR-DOC-02 §5.7 (3), §7'],
]
/** The worst outcome there is decides: one failure fails the check. */
const WORST: Outcome[] = ['failed', 'blocked', 'unmet', 'passed', 'skipped']

function trace(checks: CheckResult[]): TraceRow[] {
  return TRACE.map(([check, scenario, contract]) => {
    const results: TraceRow['results'] = {}
    for (const c of checks) if (c.id === check) results[c.outcome] = (results[c.outcome] ?? 0) + 1
    const conclusion = WORST.find((o) => results[o] !== undefined) ?? 'not_run'
    return { scenario, contract, check, owner: 'David', phase: 'DOC-1', conclusion, results }
  })
}

/** What ends a path, besides white space. */
const END = `'"\`)\\],;<>，。；：、）】》”’`
/** A path of this machine: from a root, or the home, or a drive, over the
 *  single spaces a directory's name may have (`Application Support/…`), in
 *  any case and with its slashes escaped as JSON may write them. It does not
 *  start inside a word, so the path of a URL is left alone. */
const PATH = new RegExp(`(?<![\\w.])(?:(?:~|\\\\?/(?:Users|Volumes|home|root|private|tmp|var|opt|etc|usr|mnt|srv|nix|run|data|Library|Applications|System))\\\\?/|\\b[A-Z]:\\\\)[^\\s${END}]*(?: [^\\s/${END}]+/[^\\s${END}]*)*`, 'gi')
/** A problem as it may be written down: without the paths of this machine,
 *  and without the secrets. The paths go first: a secret may be the start of
 *  one (the home), and what is left of a path with its start taken out is no
 *  longer seen as one. */
export function scrub(text: string, secrets: string[]): string {
  // The colon or full stop a path ends on belongs to the sentence.
  let out = text.replace(PATH, (found) => `<path>${/[:.]+$/.exec(found)?.[0] ?? ''}`)
  // A secret goes whatever its case (a host's name is said in any), and with its slashes escaped as JSON may write them.
  for (const secret of secrets) if (secret !== '') out = out.replace(new RegExp(secret.replace(/[.*+?^${}()|[\]\\]/g, '\\$&').replaceAll('/', '\\\\?/'), 'gi'), '<redacted>')
  return out
}

const sha256 = (file: string): string => `sha256:${crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')}`

function sampleReport(run: SampleRun, sample: Sample, clean: (c: CheckResult) => CheckResult): SampleReport {
  const find = run.find
  return {
    sample: sample.id,
    content_sha256: `sha256:${sample.meta.sha256_stored}`,
    gold_sha256: sha256(path.join(sample.dir, 'gold.json')),
    ask_sha256: sha256(path.join(sample.dir, 'ask.json')),
    document_id: run.documentId,
    extraction_id: run.extraction?.extractionId,
    model_id: run.extraction?.modelId,
    extractor_version: run.extraction?.extractorVersion,
    checks: run.checks.map(clean),
    requeued: run.requeued,
    interrupted: run.interrupted,
    text: run.text,
    ...(find ? { find: { anchors: find.anchors, reachable: find.reachable, unreached: find.located.filter((l) => l.found.length === 0).map(({ found: _, ...rest }) => rest) } } : {}),
    fields: run.scored.map(({ gold, score, result, anchors }) => ({
      key: gold.key,
      role: gold.role ?? 'answer',
      severity: gold.severity,
      gold: { status: gold.status, value: gold.value, normalized: gold.normalized, missing_reason: gold.missing_reason, candidates: gold.candidates?.map(({ value, normalized }) => ({ value, normalized })) },
      result,
      score,
      anchors,
    })),
    covers: sample.meta.covers,
    coverage: run.coverage,
    must_not_assert: sample.gold.must_not_assert.map((m, i) => ({ n: i + 1, ...m, verdict: 'unreviewed' })),
  }
}

/** The report of a run on these samples. */
export function buildReport(run: Run, samples: Sample[], ran: Ran): Report {
  const clean = (c: CheckResult): CheckResult => ({ ...c, problems: c.problems.map((p) => scrub(p, ran.secrets)) })
  const reports = run.samples.map((s) => {
    const sample = samples.find((x) => x.id === s.sample)
    if (!sample) throw new Error(`the run has a sample that was not given: ${s.sample}`)
    return sampleReport(s, sample, clean)
  })
  const { capabilities, ...capability } = run.capabilities
  const said = (of: (s: SampleReport) => string | undefined) => [...new Set(reports.flatMap((s) => of(s) ?? []))]
  const goldAnchors = (id: string) => (samples.find((x) => x.id === id)?.gold.fields ?? []).reduce((n, f) => n + (f.anchors?.length ?? 0) + (f.candidates ?? []).reduce((m, c) => m + c.anchors.length, 0), 0)
  return {
    schema: 'documents-accept-report-v1',
    generated_at: ran.at.toISOString(),
    environment: {
      commit: ran.commit,
      machine: ran.machine,
      os_version: capabilities?.os_version,
      engines: (capabilities?.engines ?? []).map(({ id, kind, state, version }) => ({ id, kind, state, version: version ?? null })),
      model_ids: said((s) => s.model_id),
      extractor_versions: said((s) => s.extractor_version),
    },
    capabilities: clean(capability),
    samples: reports,
    unverified: run.unverified,
    idempotency: run.idempotency,
    totals: run.totals,
    find: {
      anchors: reports.reduce((n, s) => n + (s.find?.anchors ?? goldAnchors(s.sample)), 0),
      reachable: reports.reduce((n, s) => n + (s.find?.reachable ?? 0), 0),
      unread: reports.filter((s) => s.find === undefined).length,
    },
    trace: trace([capability, ...run.samples.flatMap((s) => s.checks)]),
  }
}
