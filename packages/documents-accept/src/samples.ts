// What the harness reads for one S01 sample: the stored source, `meta.yaml`
// and `gold.json` (samples README §3–§4), and `ask.json`, the questions put to
// extraction (acceptance README §2). What the checks and the scoring rely on
// is verified here, so a bad sample stops a run before it starts.

import crypto from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import yaml from 'js-yaml'
import { l2 } from './text'

export type Status = 'present' | 'missing' | 'conflict'

export interface GoldAnchor {
  page: number
  quote: string
  part?: 'row' | 'column'
}

export interface GoldCandidate {
  value: string
  normalized: string
  anchors: GoldAnchor[]
}

export interface GoldField {
  key: string
  q: string
  status: Status
  severity: 'critical' | 'major' | 'minor'
  role?: 'answer' | 'context'
  value?: string
  normalized?: string
  anchors?: GoldAnchor[]
  missing_reason?: string
  candidates?: GoldCandidate[]
}

export interface Gold {
  id: string
  schema: string
  verified_by?: string
  fields: GoldField[]
  must_not_assert: { proposition: string; unless?: string }[]
}

export interface Meta {
  id: string
  pages: number
  sha256_stored: string
  questions: string[]
  covers: Record<string, string[]>
}

/** One question of `ask.json`: a `DocumentsExtractField`. */
export interface AskField {
  key: string
  description: string
  type?: string
}

export interface Sample {
  id: string
  dir: string
  /** Path of the stored source file. */
  source: string
  meta: Meta
  gold: Gold
}

const KEY = /^[a-z][a-z0-9_]{0,63}$/
const SEVERITIES = ['critical', 'major', 'minor']
const MISSING_REASONS = ['blank_in_template', 'not_in_document', 'referenced_but_absent', 'outside_page_scope']
/** What a field of each status says; a member of another status is refused. */
const MEMBERS = new Map([['present', ['value', 'normalized', 'anchors']], ['missing', ['missing_reason']], ['conflict', ['candidates']]])
const ASK_TYPES = ['text', 'date', 'datetime', 'amount', 'number', 'identifier', 'boolean']
const ASK_KEYS = ['key', 'description', 'type']
/** `find` takes at most this many characters, and every gold quote is looked up with it. */
const MAX_QUOTE = 500
const MAX_DESCRIPTION = 500
const MAX_ASKED = 100

// The files are hand-written, so nothing about their shape is taken on trust.
const broken = (x: unknown): boolean => typeof x !== 'object' || x === null || Array.isArray(x)
const isText = (x: unknown): x is string => typeof x === 'string' && x !== ''

/** Only gold that David has checked counts (samples README §5, step 3). */
export function isVerified(gold: Gold): boolean {
  return gold.verified_by === 'david'
}

function anchorProblems(at: string, anchors: GoldAnchor[] | undefined, pages: number): string[] {
  if (!Array.isArray(anchors) || anchors.length === 0) return [`${at}: no anchors`]
  return anchors.flatMap((a, i) => {
    const n = `${at}: anchor ${i + 1}`
    if (broken(a)) return [`${n} is not an object`]
    const out: string[] = []
    if (!Number.isInteger(a.page) || a.page < 1 || a.page > pages) {
      out.push(`${n}: page ${JSON.stringify(a.page)} is not a whole number from 1 to ${pages}`)
    }
    if (!isText(a.quote) || [...a.quote].length > MAX_QUOTE) out.push(`${n} has no quote, or one over ${MAX_QUOTE} characters`)
    if (a.part !== undefined && a.part !== 'row' && a.part !== 'column') out.push(`${n}: part ${String(a.part)}`)
    return out
  })
}

function fieldProblems(f: GoldField, meta: Meta): string[] {
  if (broken(f)) return ['gold.json: a field is not an object']
  const at = `gold field ${f.key}`
  const out: string[] = []
  if (!KEY.test(f.key ?? '')) out.push(`${at}: not a field key`)
  if (!SEVERITIES.includes(f.severity)) out.push(`${at}: severity ${f.severity}`)
  const context = f.role === 'context'
  if (f.role !== undefined && f.role !== 'answer' && !context) out.push(`${at}: role ${f.role}`)
  if (context ? f.q !== 'ctx' : !meta.questions.includes(f.q)) {
    out.push(`${at}: q ${f.q} is not one of the sample's questions`)
  }
  const said = [...MEMBERS.values()].flat().filter((m) => (f as unknown as Record<string, unknown>)[m] !== undefined)
  const stray = said.filter((m) => MEMBERS.get(f.status)?.includes(m) === false)
  if (stray.length > 0) out.push(`${at}: ${f.status}, yet it has ${stray.join(', ')}`)
  if (f.status === 'present') {
    if (!isText(f.value)) out.push(`${at}: present without a value`)
    if (f.normalized !== undefined && !isText(f.normalized)) out.push(`${at}: normalized is not text`)
    out.push(...anchorProblems(at, f.anchors, meta.pages))
  } else if (f.status === 'missing') {
    if (!MISSING_REASONS.includes(f.missing_reason ?? '')) out.push(`${at}: missing_reason ${f.missing_reason}`)
  } else if (f.status === 'conflict') {
    if (!Array.isArray(f.candidates) || f.candidates.length < 2) return [...out, `${at}: a conflict has two or more candidates`]
    f.candidates.forEach((c, i) => {
      if (broken(c)) return void out.push(`${at}: candidate ${i + 1} is not an object`)
      if (!isText(c.value) || !isText(c.normalized)) out.push(`${at}: candidate ${i + 1} lacks a value or its normalized form`)
      out.push(...anchorProblems(`${at} candidate ${i + 1}`, c.anchors, meta.pages))
    })
  } else {
    out.push(`${at}: status ${String(f.status)}`)
  }
  return out
}

function metaProblems(meta: Meta, id: string, sourceSha256: string): string[] {
  if (broken(meta)) return ['meta.yaml: not a mapping']
  const out: string[] = []
  if (meta.id !== id) out.push(`meta.yaml: id ${meta.id} is not the directory's`)
  if (!Number.isInteger(meta.pages) || meta.pages < 1) out.push(`meta.yaml: pages ${meta.pages}`)
  if (meta.sha256_stored !== sourceSha256) out.push(`meta.yaml: sha256_stored is not the source file's (${sourceSha256})`)
  if (!Array.isArray(meta.questions) || meta.questions.some((q) => !/^Q[1-6]$/.test(q))) out.push('meta.yaml: questions are Q1 to Q6')
  if (broken(meta.covers)) out.push('meta.yaml: covers maps coverage points to field keys')
  return out
}

function goldProblems(gold: Gold, meta: Meta, id: string): string[] {
  if (broken(gold)) return ['gold.json: not an object']
  const out: string[] = []
  if (gold.id !== id) out.push(`gold.json: id ${gold.id} is not the directory's`)
  if (gold.schema !== 's01-gold-v2') out.push(`gold.json: schema ${gold.schema}`)
  if (!Array.isArray(gold.fields) || gold.fields.length === 0) return [...out, 'gold.json: no fields']
  const keys = gold.fields.map((f) => f?.key)
  for (const k of new Set(keys.filter((k, i) => keys.indexOf(k) !== i))) out.push(`gold.json: field ${k} appears twice`)
  out.push(...gold.fields.flatMap((f) => fieldProblems(f, meta)))
  if (!Array.isArray(gold.must_not_assert) || gold.must_not_assert.some((m) => broken(m) || !isText(m.proposition))) {
    out.push('gold.json: must_not_assert is a list of propositions')
  }
  for (const [point, covered] of Object.entries(meta.covers)) {
    if (!/^C([1-9]|10)$/.test(point)) out.push(`meta.yaml: covers ${point} is not a coverage point`)
    if (!Array.isArray(covered)) out.push(`meta.yaml: covers ${point} is not a list of field keys`)
    else for (const k of covered) if (!keys.includes(k)) out.push(`meta.yaml: covers ${point} names ${k}, which is not a gold field`)
  }
  return out
}

function refuse(id: string, problems: string[]): void {
  if (problems.length > 0) throw new Error(`${id}:\n  ${problems.join('\n  ')}`)
}

/** Reads one sample directory, or throws with everything that is wrong with it. */
export function loadSample(dir: string): Sample {
  const id = path.basename(path.resolve(dir))
  const sources = fs.readdirSync(dir).filter((f) => /^source\.(pdf|png|jpg)$/.test(f))
  if (sources.length !== 1) refuse(id, [`expected one source.pdf, source.png or source.jpg, found ${sources.length}`])
  const source = path.join(dir, sources[0] ?? '')
  const sha256 = crypto.createHash('sha256').update(fs.readFileSync(source)).digest('hex')
  const meta = yaml.load(fs.readFileSync(path.join(dir, 'meta.yaml'), 'utf8')) as Meta
  const gold = JSON.parse(fs.readFileSync(path.join(dir, 'gold.json'), 'utf8')) as Gold
  const problems = metaProblems(meta, id, sha256)
  // The gold checks read meta.yaml's pages, questions and covers: with those
  // unusable, only meta.yaml's own problems are listed.
  const usable = !broken(meta) && Number.isInteger(meta.pages) && meta.pages >= 1 && Array.isArray(meta.questions) && !broken(meta.covers)
  if (usable) problems.push(...goldProblems(gold, meta, id))
  refuse(id, problems)
  return { id, dir, source, meta, gold }
}

/** How an answer is looked for in a question: L2, whatever the case or the spacing. */
const bare = (text: string): string => l2(text).toLowerCase().replaceAll(' ', '')

/** What is wrong with these questions for this gold: they must ask for every
 *  gold field, be a valid extraction schema, and not give an answer away. */
export function askProblems(ask: { fields?: AskField[] }, gold: Gold): string[] {
  const fields = broken(ask) ? undefined : ask.fields
  if (!Array.isArray(fields) || Object.keys(ask).length !== 1 || fields.length < 1 || fields.length > MAX_ASKED) {
    return [`ask.json: an object with 1 to ${MAX_ASKED} fields, and nothing else`]
  }
  if (fields.some(broken)) return ['ask.json: a field is not an object']
  const out: string[] = []
  const asked = fields.map((f) => f.key)
  const wanted = gold.fields.map((f) => f.key)
  for (const k of wanted) if (!asked.includes(k)) out.push(`ask.json: no question for ${k}`)
  for (const k of new Set(asked.filter((k, i) => asked.indexOf(k) !== i))) out.push(`ask.json: ${k} is asked twice`)
  for (const f of fields) {
    const at = `ask.json field ${f.key}`
    const g = gold.fields.find((x) => x.key === f.key)
    if (g === undefined) out.push(`${at}: not a gold field`)
    if (Object.keys(f).some((k) => !ASK_KEYS.includes(k))) out.push(`${at}: only key, description and type`)
    if (!isText(f.description) || [...f.description].length > MAX_DESCRIPTION) {
      out.push(`${at}: a description of 1 to ${MAX_DESCRIPTION} characters`)
      continue
    }
    if (f.type !== undefined && !ASK_TYPES.includes(f.type)) out.push(`${at}: type ${f.type}`)
    const answers = [g?.value, g?.normalized, ...(g?.candidates ?? []).flatMap((c) => [c.value, c.normalized])]
    const said = bare(f.description)
    if (answers.some((a) => a !== undefined && bare(a) !== '' && said.includes(bare(a)))) {
      out.push(`${at}: the description gives the answer away`)
    }
  }
  return out
}

/** Reads a sample's `ask.json`, or throws with everything that is wrong with it. */
export function loadAsk(sample: Sample): AskField[] {
  const ask = JSON.parse(fs.readFileSync(path.join(sample.dir, 'ask.json'), 'utf8')) as { fields?: AskField[] }
  refuse(sample.id, askProblems(ask, sample.gold))
  return ask.fields ?? []
}
