// The command line of the harness (acceptance README §5):
//
//   accept run --out <dir> [--samples <dir>] [--daemon <daemon.json>] [--only <id,id>]
//              [--replay-on <id>] [--cancel-on <id>] [--job-minutes <n>]
//   accept review --out <dir> [--daemon <daemon.json>]
//
// `run` puts the samples through a running daemon and writes report.json and
// report.md into <dir>; `review` merges <dir>/review.json into them (§4.7).

import { execFileSync } from 'node:child_process'
import crypto from 'node:crypto'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { Client, connect } from './client'
import { type Ran, type Report, buildReport, holdsPath } from './report'
import { renderMarkdown } from './report-md'
import { type ReviewItem, type Reviewed, applyReview, readReport, readReview } from './review'
import { run } from './run'
import { loadAsk, loadSample } from './samples'

const HERE = path.dirname(fileURLToPath(import.meta.url))
const USAGE = 'usage: accept run --out <dir> [--samples <dir>] [--daemon <daemon.json>] [--only <id,id>] [--replay-on <id>] [--cancel-on <id>] [--job-minutes <n>]\n       accept review --out <dir> [--daemon <daemon.json>]'

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
    if (name === undefined || (command === 'review' && name !== 'out' && name !== 'daemon')) return fail(`${flag} is no option of ${command}`)
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

const escaped = (text: string): string => text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
/** A secret wherever it is, glued to a word or not (`Bearer%20<token>`), whatever its case. */
const anywhere = (secret: string): RegExp => new RegExp(escaped(secret), 'i')
/** A name where it stands as a whole: not inside a longer name (`MacBook` in
 *  `MacBookPro18,3`), whatever its case. */
const word = (name: string): RegExp => new RegExp(`${/^[a-z0-9]/i.test(name) ? '(?<![a-z0-9])' : ''}${escaped(name)}${/[a-z0-9]$/i.test(name) ? '(?![a-z0-9])' : ''}`, 'i')

/** Where in the report the first text is that `holds` it, by its keys and
 *  never by what it says; or report.md, when only the text made of it there does. */
function where(report: unknown, holds: (text: string) => boolean): string {
  const found = (x: unknown, at: string): string | undefined => {
    if (typeof x === 'string') return holds(x) ? at : undefined
    if (typeof x !== 'object' || x === null) return undefined
    for (const [k, y] of Object.entries(x)) {
      const inside = found(y, at === '' ? k : `${at}.${k}`)
      if (inside !== undefined) return inside
    }
    return undefined
  }
  return found(report, '') ?? 'report.md'
}

/** Writes report.json and report.md, unless either holds a secret, a name of
 *  this machine or a path of it (§5): then nothing is written, and neither is
 *  said, only where it is. A secret (the bearer, the home) is found anywhere; a
 *  name only as a whole word, and not in `environment.machine`, which the
 *  harness reads off this machine itself (`Mac16` is in `Mac16,10`). Both files
 *  are checked: report.md says the text of report.json with its white space
 *  made one space, and a path written with a tab in report.json is written
 *  with a space in report.md. Each is written beside its place under a name of
 *  its own and then moved into it, so that no file is left half written; what
 *  was not moved is taken away. */
export function writeEvidence(dir: string, report: Report & { review?: Reviewed }, secrets: string[], names: string[] = []): void {
  const json = `${JSON.stringify(report, null, 2)}\n`
  const markdown = `${renderMarkdown(report)}\n`
  const unnamed = { ...report, environment: { ...report.environment, machine: { model: '', os: '' } } }
  const looked = [
    ...secrets.map((s) => ({ s, find: anywhere, in: report, texts: [json, markdown] })),
    ...names.map((s) => ({ s, find: word, in: unnamed, texts: [JSON.stringify(unnamed, null, 2), renderMarkdown(unnamed)] })),
  ]
  // Each is looked for as it is and as JSON writes it.
  const leak = looked.findIndex(({ s, find, texts }) => s !== '' && [s, JSON.stringify(s).slice(1, -1)].some((as) => texts.some((t) => find(as).test(t))))
  if (leak !== -1) {
    const { s, find, in: of } = looked[leak]!
    throw new Error(`the report holds what it must not (secret ${leak + 1} of ${looked.length}, at ${where(of, (t) => find(s).test(t))}): nothing was written`)
  }
  if (holdsPath(json) || holdsPath(markdown)) throw new Error(`the report holds a path of this machine (at ${where(report, holdsPath)}): nothing was written`)
  fs.mkdirSync(dir, { recursive: true })
  const files = [['report.json', json], ['report.md', markdown]].map(([name, text]) => ({ name: name!, text: text!, temp: path.join(dir, `.${name}.${crypto.randomUUID()}.tmp`) }))
  try {
    for (const f of files) fs.writeFileSync(f.temp, f.text)
    for (const f of files) fs.renameSync(f.temp, path.join(dir, f.name))
  } catch (error) {
    // What could not be taken away does not hide why the writing failed.
    for (const f of files) {
      try {
        fs.rmSync(f.temp, { force: true })
      } catch {}
    }
    throw error
  }
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

/** What of this machine must not be in a report: the user's home, and its
 *  name, with and without its domain, however short. */
function ofThisMachine(): { secrets: string[]; names: string[] } {
  const host = os.hostname()
  return { secrets: [os.homedir()], names: [...new Set([host, host.split('.')[0] ?? ''])].filter((name) => name !== '') }
}

/** The daemon's bearer, for a review to be held to it too: from the state file
 *  named, or else from the usual one, where there is one with a bearer. */
function bearer(daemon: string | undefined): string {
  if (daemon !== undefined) return connect(daemon).token
  try {
    return connect().token
  } catch {
    return ''
  }
}

/** An item of a review by what it is about, without its verdict or note: what the reviewer wrote is not said back. */
const about = (item: ReviewItem): string =>
  [item.sample, item.field, item.anchor === undefined ? undefined : `anchor ${item.anchor}`, item.must_not_assert === undefined ? undefined : `must_not_assert ${item.must_not_assert}`].filter((x) => x !== undefined).join(' · ')

/** What this machine and this checkout are, as §5 asks; a tree with changes is said to be dirty. */
function ran(token: string): Ran {
  const commit = tool('git', ['rev-parse', '--short=12', 'HEAD']) || 'unknown'
  const dirty = tool('git', ['status', '--porcelain']) !== ''
  const version = tool('sw_vers', ['-productVersion'])
  return {
    commit: dirty ? `${commit}-dirty` : commit,
    machine: { model: tool('sysctl', ['-n', 'hw.model']) || os.arch(), os: version ? `macOS ${version}` : `${os.type()} ${os.release()}` },
    at: new Date(),
    secrets: [token, ...ofThisMachine().secrets, ...ofThisMachine().names],
  }
}

/** Runs one command; what it prints, and the code to exit with. */
export async function main(argv: string[], print: (line: string) => void = console.log): Promise<number> {
  const args = parseArgs(argv)
  if (args.command === 'review') {
    const report = readReport(JSON.parse(fs.readFileSync(path.join(args.out, 'report.json'), 'utf8')))
    const merged = applyReview(report, readReview(JSON.parse(fs.readFileSync(path.join(args.out, 'review.json'), 'utf8'))))
    const machine = ofThisMachine()
    writeEvidence(args.out, merged, [bearer(args.daemon), ...machine.secrets], machine.names)
    print(`merged the review of ${merged.review.reviewer}: ${merged.review.applied} decisions applied, ${merged.review.unused.length} not`)
    for (const u of merged.review.unused) print(`  not applied: ${about(u.item)} (${u.why})`)
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
  const machine = ofThisMachine()
  writeEvidence(args.out, report, [connection.token, ...machine.secrets], machine.names)
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
