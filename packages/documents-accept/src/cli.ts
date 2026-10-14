// The command line of the harness (acceptance README §5):
//
//   accept run --out <dir> [--samples <dir>] [--daemon <daemon.json>] [--only <id,id>]
//              [--replay-on <id>] [--cancel-on <id>] [--job-minutes <n>]
//   accept review --out <dir>
//
// `run` puts the samples through a running daemon and writes report.json and
// report.md into <dir>; `review` merges <dir>/review.json into them (§4.7).

import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { Client, connect } from './client'
import { type Ran, type Report, buildReport } from './report'
import { renderMarkdown } from './report-md'
import { type Reviewed, applyReview, readReport, readReview } from './review'
import { run } from './run'
import { loadAsk, loadSample } from './samples'

const HERE = path.dirname(fileURLToPath(import.meta.url))
const USAGE = 'usage: accept run --out <dir> [--samples <dir>] [--daemon <daemon.json>] [--only <id,id>] [--replay-on <id>] [--cancel-on <id>] [--job-minutes <n>]\n       accept review --out <dir>'

export interface Args {
  command: 'run' | 'review'
  out: string
  samples: string
  daemon?: string
  only?: string[]
  replayOn: string
  cancelOn: string
  jobMinutes: number
}

const FLAGS: Record<string, keyof Args> = { '--out': 'out', '--samples': 'samples', '--daemon': 'daemon', '--only': 'only', '--replay-on': 'replayOn', '--cancel-on': 'cancelOn', '--job-minutes': 'jobMinutes' }

/** The arguments as given, or an error with the usage. §3 names the samples the idempotency checks run on. */
export function parseArgs(argv: string[]): Args {
  const fail = (why: string): never => {
    throw new Error(`${why}\n${USAGE}`)
  }
  const [command, ...rest] = argv
  if (command !== 'run' && command !== 'review') return fail(`${command ?? 'nothing'} is no command`)
  const given: Record<string, string> = {}
  for (let i = 0; i < rest.length; i += 2) {
    const [flag, value] = [rest[i] ?? '', rest[i + 1]]
    const name = FLAGS[flag]
    if (name === undefined || (command === 'review' && name !== 'out')) return fail(`${flag} is no option of ${command}`)
    if (value === undefined || value === '' || value.startsWith('--')) return fail(`${flag} needs a value`)
    if (name in given) return fail(`${flag} is given twice`)
    given[name] = value
  }
  if (given.out === undefined) return fail('--out is needed: the directory of the evidence')
  const jobMinutes = /^[0-9]+$/.test(given.jobMinutes ?? '30') ? Number(given.jobMinutes ?? 30) : 0
  if (jobMinutes < 1) return fail('--job-minutes needs a whole number of minutes')
  return {
    command,
    out: given.out,
    samples: given.samples ?? path.join(HERE, '..', '..', '..', 'docs', 'documenting', 'samples', 's01'),
    ...(given.daemon === undefined ? {} : { daemon: given.daemon }),
    ...(given.only === undefined ? {} : { only: given.only.split(',') }),
    replayOn: given.replayOn ?? 's01-07-mixed-building-notice',
    cancelOn: given.cancelOn ?? 's01-01-zh-sh-fee-2025',
    jobMinutes,
  }
}

/** Writes report.json and report.md, unless the report holds a secret (§5):
 *  then nothing is written, and the secret is not said. */
export function writeEvidence(dir: string, report: Report & { review?: Reviewed }, secrets: string[]): void {
  const json = `${JSON.stringify(report, null, 2)}\n`
  const markdown = `${renderMarkdown(report)}\n`
  // A secret is looked for as it is and as JSON writes it, whatever its case.
  const written = (json + markdown).toLowerCase()
  const leak = secrets.findIndex((s) => s !== '' && [s, JSON.stringify(s).slice(1, -1)].some((as) => written.includes(as.toLowerCase())))
  if (leak !== -1) throw new Error(`the report holds what it must not (secret ${leak + 1} of ${secrets.length}): nothing was written`)
  fs.mkdirSync(dir, { recursive: true })
  fs.writeFileSync(path.join(dir, 'report.json'), json)
  fs.writeFileSync(path.join(dir, 'report.md'), markdown)
}

/** 0: every check passed or had nothing to do. 2: the daemon could not run them. 1: anything else. */
export function exitCode(report: Report): number {
  if (report.capabilities.outcome === 'unmet') return 2
  return report.trace.every((t) => t.conclusion === 'passed' || t.conclusion === 'skipped') ? 0 : 1
}

const tool = (command: string, args: string[]): string => {
  try {
    return execFileSync(command, args, { cwd: HERE, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim()
  } catch {
    return ''
  }
}

/** What of this machine must not be in a report: its name, with and without its domain, and the user's home. */
function ofThisMachine(): string[] {
  const host = os.hostname()
  const label = host.split('.')[0] ?? ''
  return [host, label.length >= 6 ? label : '', os.homedir()]
}

/** What this machine and this checkout are, as §5 asks; a tree with changes is said to be dirty. */
function ran(token: string): Ran {
  const commit = tool('git', ['rev-parse', '--short=12', 'HEAD']) || 'unknown'
  const dirty = tool('git', ['status', '--porcelain']) !== ''
  const version = tool('sw_vers', ['-productVersion'])
  return {
    commit: dirty ? `${commit}-dirty` : commit,
    machine: { model: tool('sysctl', ['-n', 'hw.model']) || os.arch(), os: version ? `macOS ${version}` : `${os.type()} ${os.release()}` },
    at: new Date(),
    secrets: [token, ...ofThisMachine()],
  }
}

/** Runs one command; what it prints, and the code to exit with. */
export async function main(argv: string[], print: (line: string) => void = console.log): Promise<number> {
  const args = parseArgs(argv)
  if (args.command === 'review') {
    const report = readReport(JSON.parse(fs.readFileSync(path.join(args.out, 'report.json'), 'utf8')))
    const merged = applyReview(report, readReview(JSON.parse(fs.readFileSync(path.join(args.out, 'review.json'), 'utf8'))))
    writeEvidence(args.out, merged, ofThisMachine())
    print(`merged the review of ${merged.review.reviewer}: ${merged.review.applied} decisions applied, ${merged.review.unused.length} not`)
    for (const u of merged.review.unused) print(`  not applied: ${JSON.stringify(u.item)} (${u.why})`)
    return 0
  }
  const ids = fs.readdirSync(args.samples).filter((d) => fs.existsSync(path.join(args.samples, d, 'gold.json'))).sort()
  const unknown = (args.only ?? []).filter((id) => !ids.includes(id))
  if (unknown.length > 0 || ids.length === 0) throw new Error(ids.length === 0 ? `${args.samples} has no samples` : `no such sample: ${unknown.join(', ')}`)
  const chosen = ids.filter((id) => args.only === undefined || args.only.includes(id))
  // Named on the command line, a sample for the idempotency checks must be one that runs; the defaults of §3 need not be.
  const absent = (['--replay-on', '--cancel-on'] as const).filter((flag, i) => argv.includes(flag) && !chosen.includes([args.replayOn, args.cancelOn][i] ?? ''))
  if (absent.length > 0) throw new Error(`${absent.join(' and ')} must name a sample of this run`)
  const samples = chosen.map((id) => loadSample(path.join(args.samples, id)))
  const connection = connect(args.daemon)
  const done = await run(new Client(connection), samples.map((sample) => ({ sample, ask: loadAsk(sample) })), { replayOn: args.replayOn, cancelOn: args.cancelOn, timeoutMs: args.jobMinutes * 60_000 })
  const by = ran(connection.token)
  const report = buildReport(done, samples, by)
  writeEvidence(args.out, report, by.secrets)
  for (const t of report.trace) print(`${t.check.padEnd(12)} ${t.conclusion}`)
  print(`report.json and report.md are in ${args.out}`)
  return exitCode(report)
}

if (process.argv[1] !== undefined && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).then(
    (code) => process.exit(code),
    (error: unknown) => {
      console.error(error instanceof Error ? error.message : String(error))
      process.exit(1)
    },
  )
}
