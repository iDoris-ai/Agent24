import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import yaml from 'js-yaml'
import { afterEach, describe, expect, it } from 'vitest'
import { type AskField, type Gold, type Meta, askProblems, isVerified, loadAsk, loadSample } from './samples'

const S01 = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..', 'docs', 'documenting', 'samples', 's01')
const SMALL = 's01-07-mixed-building-notice'

const made: string[] = []
afterEach(() => {
  for (const dir of made.splice(0)) fs.rmSync(dir, { recursive: true, force: true })
})

/** A copy of the one-page sample, with its gold and meta changed by `change`. */
function copy(change: (gold: Gold, meta: Meta, dir: string) => void, name = SMALL): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'accept-'))
  made.push(root)
  const dir = path.join(root, name)
  fs.cpSync(path.join(S01, SMALL), dir, { recursive: true })
  const gold = JSON.parse(fs.readFileSync(path.join(dir, 'gold.json'), 'utf8')) as Gold
  const meta = yaml.load(fs.readFileSync(path.join(dir, 'meta.yaml'), 'utf8')) as Meta
  change(gold, meta, dir)
  fs.writeFileSync(path.join(dir, 'gold.json'), JSON.stringify(gold))
  fs.writeFileSync(path.join(dir, 'meta.yaml'), yaml.dump(meta))
  return dir
}

/** A copy of the one-page sample, with `file` replaced by `text` as it stands. */
function withFile(file: string, text: string): string {
  const dir = copy(() => {})
  fs.writeFileSync(path.join(dir, file), text)
  return dir
}

function field(gold: Gold, key: string) {
  const f = gold.fields.find((x) => x.key === key)
  if (f === undefined) throw new Error(`no gold field ${key}`)
  return f
}

describe('the S01 samples in this repository', () => {
  const samples = fs.readdirSync(S01).filter((d) => d.startsWith('s01-')).map((d) => loadSample(path.join(S01, d)))

  it('all load, and David has verified every one', () => {
    expect(samples.map((s) => s.id)).toHaveLength(8)
    expect(samples.every((s) => isVerified(s.gold))).toBe(true)
    expect(isVerified({ ...samples[0]!.gold, verified_by: 'codex' })).toBe(false)
    expect(isVerified({ ...samples[0]!.gold, verified_by: undefined })).toBe(false)
  })

  // These are the numbers docs/documenting/acceptance/README.md quotes (§4.3, §4.4, §4.8).
  // A gold change that moves them is fine: update them here and there, in the same PR.
  it('hold the fields and anchors the acceptance design quotes', () => {
    const fields = samples.flatMap((s) => s.gold.fields)
    expect(fields).toHaveLength(150)
    expect(fields.filter((f) => f.status === 'present')).toHaveLength(128)
    expect(fields.filter((f) => f.status === 'missing')).toHaveLength(20)
    expect(fields.filter((f) => f.status === 'conflict')).toHaveLength(2)
    const anchors = fields.flatMap((f) => [...(f.anchors ?? []), ...(f.candidates ?? []).flatMap((c) => c.anchors)])
    expect(anchors).toHaveLength(277)
  })
})

describe('loadSample refuses', () => {
  const refused = (change: Parameters<typeof copy>[0], name?: string) => () => loadSample(copy(change, name))

  it('a copy that is unchanged is accepted', () => {
    expect(loadSample(copy(() => {})).gold.fields).toHaveLength(20)
  })

  it('a source file that is not the one meta.yaml records', () => {
    expect(refused((_g, _m, dir) => fs.appendFileSync(path.join(dir, 'source.pdf'), 'x'))).toThrow(/sha256_stored is not the source file's/)
  })

  it('two source files, or none', () => {
    expect(refused((_g, _m, dir) => fs.writeFileSync(path.join(dir, 'source.png'), 'x'))).toThrow(/found 2/)
    expect(refused((_g, _m, dir) => fs.rmSync(path.join(dir, 'source.pdf')))).toThrow(/found 0/)
  })

  it('an id that is not the directory name', () => {
    expect(refused(() => {}, 's01-99-other')).toThrow(/meta.yaml: id .* is not the directory's/)
    expect(refused((g) => void (g.id = 's01-99-other'))).toThrow(/gold.json: id .* is not the directory's/)
  })

  it('gold of another schema, or with a field twice', () => {
    expect(refused((g) => void (g.schema = 's01-gold-v1'))).toThrow(/schema s01-gold-v1/)
    expect(refused((g) => void g.fields.push(field(g, 'contact_phone')))).toThrow(/field contact_phone appears twice/)
  })

  it('a field whose status, severity, role or question is not one the spec has', () => {
    expect(refused((g) => void (field(g, 'contact_phone').status = 'absent' as never))).toThrow(/contact_phone: status absent/)
    // A name every object has is no status either, and is refused like any other.
    expect(refused((g) => void (field(g, 'contact_phone').status = 'constructor' as never))).toThrow(/contact_phone: status constructor/)
    expect(refused((g) => void (field(g, 'contact_phone').severity = 'fatal' as never))).toThrow(/contact_phone: severity fatal/)
    expect(refused((g) => void (field(g, 'contact_phone').role = 'extra' as never))).toThrow(/contact_phone: role extra/)
    expect(refused((_g, m) => void (m.questions = ['Q1']))).toThrow(/q Q2 is not one of the sample's questions/)
    expect(refused((_g, m) => void (m.questions = ['Q1', 'Q7']))).toThrow(/meta.yaml: questions are Q1 to Q6/)
    expect(refused((_g, m) => void (m.questions = 'Q1' as never))).toThrow(/meta.yaml: questions are Q1 to Q6/)
    expect(refused((g) => void (field(g, 'notice_reference').q = 'Q1'))).toThrow(/notice_reference: q Q1 is not/)
  })

  it('a present field without a value or without anchors, or whose value is not text', () => {
    expect(refused((g) => void delete field(g, 'work_order').value)).toThrow(/work_order: present without a value/)
    expect(refused((g) => void (field(g, 'work_order').value = 731 as never))).toThrow(/work_order: present without a value/)
    expect(refused((g) => void (field(g, 'notice_date').normalized = 20261008 as never))).toThrow(/notice_date: normalized is not text/)
    expect(refused((g) => void (field(g, 'work_order').anchors = []))).toThrow(/work_order: no anchors/)
  })

  it('an anchor off the document, or with a quote find cannot take', () => {
    const anchored = (anchor: object) => refused((g) => void (field(g, 'work_order').anchors = [anchor as never]))
    expect(anchored({ page: 2, quote: 'WO-000731' })).toThrow(/anchor 1: page 2 is not a whole number from 1 to 1/)
    expect(anchored({ page: 0, quote: 'WO-000731' })).toThrow(/page 0 is not a whole number/)
    expect(anchored({ page: '1', quote: 'WO-000731' })).toThrow(/page "1" is not a whole number/)
    expect(anchored({ page: 1, quote: 731 })).toThrow(/anchor 1 has no quote/)
    expect(anchored({ page: 1, quote: 'WO-000731', part: 'cell' })).toThrow(/anchor 1: part cell/)
    expect(refused((g) => void (field(g, 'work_order').anchors = [{ page: 1, quote: '号'.repeat(501) }]))).toThrow(/over 500 characters/)
    expect(loadSample(copy((g) => void (field(g, 'work_order').anchors = [{ page: 1, quote: '号'.repeat(500) }]))).id).toBe(SMALL)
  })

  it('a field that says what only another status says', () => {
    expect(refused((g) => void (field(g, 'contact_phone').value = '021-5555'))).toThrow(/contact_phone: missing, yet it has value/)
    expect(refused((g) => void Object.assign(field(g, 'shutoff_date'), { value: '10月13日', anchors: [] }))).toThrow(/shutoff_date: conflict, yet it has value, anchors/)
    expect(refused((g) => void Object.assign(field(g, 'work_order'), { candidates: [], missing_reason: 'unread' }))).toThrow(/work_order: present, yet it has missing_reason, candidates/)
  })

  it('a missing field without one of the four reasons', () => {
    expect(refused((g) => void (field(g, 'contact_phone').missing_reason = 'unread'))).toThrow(/contact_phone: missing_reason unread/)
  })

  it('a conflict with one candidate, or a candidate without its normalized form or anchors', () => {
    expect(refused((g) => void field(g, 'shutoff_date').candidates?.splice(1))).toThrow(/a conflict has two or more candidates/)
    expect(refused((g) => void (field(g, 'shutoff_date').candidates![0]!.normalized = ''))).toThrow(/candidate 1 lacks a value or its normalized form/)
    // Blank is as good as nothing: the scoring groups candidates by this form.
    expect(refused((g) => void (field(g, 'shutoff_date').candidates![0]!.normalized = ' '))).toThrow(/candidate 1 lacks a value or its normalized form/)
    expect(refused((g) => void (field(g, 'shutoff_date').candidates![0]!.value = 13 as never))).toThrow(/candidate 1 lacks a value or its normalized form/)
    expect(refused((g) => void (field(g, 'shutoff_date').candidates![1]!.anchors = []))).toThrow(/shutoff_date candidate 2: no anchors/)
  })

  it('a coverage point that is not one, or that names a field the gold lacks', () => {
    expect(refused((_g, m) => void (m.covers = { C11: ['contact_phone'] }))).toThrow(/covers C11 is not a coverage point/)
    expect(refused((_g, m) => void (m.covers = { C9: ['contact_fax'] }))).toThrow(/covers C9 names contact_fax/)
    expect(refused((_g, m) => void (m.covers = { C9: 'contact_phone' as never }))).toThrow(/covers C9 is not a list of field keys/)
    expect(refused((_g, m) => void (m.covers = 'C9' as never))).toThrow(/covers maps coverage points to field keys/)
  })

  it('a meta.yaml without a page count', () => {
    expect(refused((_g, m) => void (m.pages = 0))).toThrow(/meta.yaml: pages 0/)
    expect(refused((_g, m) => void (m.pages = 'one' as never))).toThrow(/meta.yaml: pages one/)
  })

  it('a file that is not shaped as the spec says, with a problem and not a crash', () => {
    expect(() => loadSample(withFile('gold.json', 'null'))).toThrow(/gold.json: not an object/)
    expect(() => loadSample(withFile('meta.yaml', 'just text'))).toThrow(/meta.yaml: not a mapping/)
    expect(refused((g) => void g.fields.push(null as never))).toThrow(/gold.json: a field is not an object/)
    expect(refused((g) => void (g.must_not_assert = [null as never]))).toThrow(/must_not_assert is a list of propositions/)
    expect(refused((g) => void (g.must_not_assert = [{ proposition: '' }]))).toThrow(/must_not_assert is a list of propositions/)
    expect(refused((g) => void (field(g, 'work_order').anchors = [null as never]))).toThrow(/work_order: anchor 1 is not an object/)
    expect(refused((g) => void (field(g, 'shutoff_date').candidates = [null, null] as never))).toThrow(/shutoff_date: candidate 2 is not an object/)
  })

  it('everything that is wrong at once, not only the first', () => {
    const both = refused((g) => {
      field(g, 'work_order').anchors = []
      field(g, 'contact_phone').missing_reason = 'unread'
    })
    expect(both).toThrow(/work_order: no anchors\n.*contact_phone: missing_reason unread/s)
  })

  it('what is wrong with meta.yaml and with gold.json together', () => {
    const both = refused((g, _m, dir) => {
      fs.appendFileSync(path.join(dir, 'source.pdf'), 'x')
      field(g, 'work_order').anchors = []
    })
    expect(both).toThrow(/sha256_stored is not the source file's.*\n.*work_order: no anchors/s)
  })
})

describe('ask.json', () => {
  const gold = loadSample(path.join(S01, SMALL)).gold
  const ask = (change: (fields: AskField[]) => void = () => {}) => {
    const fields: AskField[] = gold.fields.map((f) => ({ key: f.key, description: `What the notice says about ${f.key}`, type: 'text' }))
    change(fields)
    return { fields }
  }
  const one = (fields: AskField[], key: string) => fields.find((f) => f.key === key)!

  it('with one question per gold field is accepted', () => {
    expect(askProblems(ask(), gold)).toEqual([])
    expect(askProblems(ask((f) => void delete one(f, 'work_order').type), gold)).toEqual([])
  })

  it('must ask for every gold field, once, and for nothing else', () => {
    expect(askProblems(ask((f) => void f.pop()), gold)).toEqual([`ask.json: no question for ${gold.fields.at(-1)!.key}`])
    expect(askProblems(ask((f) => void f.push(f[0]!)), gold)).toEqual([`ask.json: ${gold.fields[0]!.key} is asked twice`])
    expect(askProblems(ask((f) => void f.push({ key: 'contact_fax', description: 'The fax number' })), gold)).toEqual(['ask.json field contact_fax: not a gold field'])
  })

  it('must be what the extraction route takes', () => {
    expect(askProblems({ ...ask(), revision: 1 } as never, gold)).toEqual(['ask.json: an object with 1 to 100 fields, and nothing else'])
    expect(askProblems({ fields: [] }, gold)).toHaveLength(1)
    const many = Array.from({ length: 101 }, (_, i) => ({ key: `f${i}`, description: 'x' }))
    expect(askProblems({ fields: many }, gold)).toEqual(['ask.json: an object with 1 to 100 fields, and nothing else'])
    expect(askProblems(null as never, gold)).toHaveLength(1)
    expect(askProblems(ask((f) => void f.push(null as never)), gold)).toEqual(['ask.json: a field is not an object'])
    expect(askProblems(ask((f) => void (one(f, 'work_order').description = 731 as never)), gold)).toHaveLength(1)
    expect(askProblems(ask((f) => void (one(f, 'work_order').type = 'phone')), gold)).toEqual(['ask.json field work_order: type phone'])
    expect(askProblems(ask((f) => void (one(f, 'work_order').description = '')), gold)).toEqual(['ask.json field work_order: a description of 1 to 500 characters'])
    expect(askProblems(ask((f) => void (one(f, 'work_order').description = '号'.repeat(501))), gold)).toHaveLength(1)
    expect(askProblems(ask((f) => void Object.assign(one(f, 'work_order'), { required: true })), gold)).toEqual(['ask.json field work_order: only key, description and type'])
  })

  it('must not give an answer away: the value, its normalized form, or a candidate and its', () => {
    const leak = (key: string, description: string) => askProblems(ask((f) => void (one(f, key).description = description)), gold)
    expect(leak('work_order', '工单号是不是 WO-000731')).toEqual(['ask.json field work_order: the description gives the answer away'])
    expect(leak('notice_date', '通知是 2026-10-08 发的吗')).toHaveLength(1)
    expect(leak('shutoff_date', '停水是不是在10月13日')).toHaveLength(1)
    // The same answer, spaced as a text layer might space it.
    expect(leak('shutoff_date', '停水是不是在 10 月 13 日')).toHaveLength(1)
    expect(leak('shutoff_date', '停水是不是 2026-10-13')).toHaveLength(1)
    // Nor by changing its case or spacing it out.
    expect(leak('work_order', 'is the work order wo-000731')).toHaveLength(1)
    expect(leak('work_order', 'is the work order WO - 000731')).toHaveLength(1)
    // Another field's answer is not this field's.
    expect(leak('work_order', '通知编号 FM-2026-0042 对应的工单号')).toEqual([])
  })

  it('is read from the sample directory', () => {
    const dir = copy((_g, _m, d) => fs.writeFileSync(path.join(d, 'ask.json'), JSON.stringify(ask())))
    expect(loadAsk(loadSample(dir))).toHaveLength(gold.fields.length)
    fs.writeFileSync(path.join(dir, 'ask.json'), JSON.stringify(ask((f) => void f.pop())))
    expect(() => loadAsk(loadSample(dir))).toThrow(/no question for/)
  })
})
