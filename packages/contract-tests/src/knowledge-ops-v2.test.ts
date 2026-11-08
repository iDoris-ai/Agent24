// KB-CT-03③ (#1668): the knowledge entry part of CONTRACT-OPS-v2 (§8). Every
// case in protocol/knowledge/fixtures/ops-v2/schema-cases.json must get its
// verdict from the ops-v2 schemas, and every hash sample must recompute under
// scheme 1 (W8.13). The Rust side (agent24-protocol tests/knowledge_ops_v2.rs)
// runs the same files through jsonschema, so the two engines are held to the
// same verdicts. Pure schema test, no daemon.

import { beforeAll, describe, expect, it } from 'vitest'
import { createHash } from 'node:crypto'
import { readFileSync, readdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import Ajv2020 from 'ajv/dist/2020.js'

type Validator = ((doc: unknown) => boolean) & { errors?: unknown }
type Case = { name: string; schema: string; valid: boolean; doc: unknown }
type Sample = { name: string; kind: string; title: string; body: string; sha256: string; serialized: string }

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')
const schemaDir = join(repoRoot, 'protocol', 'knowledge', 'ops-v2')
const fixtureDir = join(repoRoot, 'protocol', 'knowledge', 'fixtures', 'ops-v2')
const BASE = 'https://github.com/iDoris-ai/Agent24/protocol/knowledge/ops-v2/'
// Pins both fixture files. Changing one means re-pinning here and in
// rust/crates/agent24-protocol/tests/knowledge_ops_v2.rs together.
const FIXTURE_SHA256: Record<string, string> = {
  'schema-cases.json': '127430af1ff6d883c02fc75bd267ed3377da40d8a3bc8fa1dad72f3552db2f67',
  'entry-hash.samples.json': 'c8069d60b6ee2e5789aa2cfa7a4dfb1c08228cfef16ca78bbdf8fedc2cbf5b2e',
}
const load = (file: string): any => JSON.parse(readFileSync(join(fixtureDir, file), 'utf8'))
const v: Record<string, Validator> = {}
const DEFS: Record<string, string[]> = {
  candidate: ['submit_request', 'submit_response', 'withdraw_request', 'withdraw_response', 'return_request', 'return_response', 'approve_request', 'approve_response', 'view'],
  entry: ['withdraw_request', 'withdraw_response'],
}

beforeAll(() => {
  const ajv = new Ajv2020.default({ strictSchema: true, strictTypes: false, allErrors: true })
  for (const f of readdirSync(schemaDir)) ajv.addSchema(JSON.parse(readFileSync(join(schemaDir, f), 'utf8')))
  for (const [file, defs] of Object.entries(DEFS)) {
    for (const d of defs) v[`${file}.${d}`] = ajv.compile({ $ref: `${BASE}${file}.schema.json#/$defs/${d}` }) as Validator
  }
})

describe('knowledge ops v2 fixtures', () => {
  it('pins exactly the files in the directory', () => {
    expect(readdirSync(fixtureDir).sort()).toEqual(Object.keys(FIXTURE_SHA256).sort())
    for (const [file, want] of Object.entries(FIXTURE_SHA256)) {
      const actual = createHash('sha256').update(readFileSync(join(fixtureDir, file))).digest('hex')
      expect(actual, `${file}: changed; re-pin it here and in knowledge_ops_v2.rs`).toBe(want)
    }
  })

  it('gives every schema case its verdict', () => {
    const cases = load('schema-cases.json').cases as Case[]
    expect(cases.length).toBeGreaterThan(40)
    const names = new Set<string>()
    for (const c of cases) {
      expect(names.has(c.name), `duplicate case ${c.name}`).toBe(false)
      names.add(c.name)
      expect(v[c.schema], `${c.name}: unknown schema ${c.schema}`).toBeDefined()
      expect(v[c.schema](c.doc), `${c.name}: ${JSON.stringify(v[c.schema].errors)}`).toBe(c.valid)
    }
    // Every entry point has at least one accepted case.
    for (const key of Object.keys(v)) expect(cases.some((c) => c.schema === key && c.valid), key).toBe(true)
  })

  it('recomputes every entry hash sample under scheme 1 (W8.13)', () => {
    const fx = load('entry-hash.samples.json') as { scheme: number; samples: Sample[] }
    expect(fx.scheme).toBe(1)
    expect(fx.samples.some((s) => /[一-鿿]/.test(s.title))).toBe(true)
    expect(fx.samples.some((s) => /\p{Extended_Pictographic}/u.test(s.title + s.body))).toBe(true)
    for (const s of fx.samples) {
      const serialized = JSON.stringify([1, s.kind, s.title, s.body])
      expect(serialized, s.name).toBe(s.serialized)
      expect(createHash('sha256').update(serialized, 'utf8').digest('hex'), s.name).toBe(s.sha256)
    }
  })

  it('keeps the §6 enumerations of the contract and the schemas the same', () => {
    const contract = readFileSync(join(repoRoot, 'docs', 'knowledge', 'CONTRACT-OPS-v2.md'), 'utf8')
    const row = (label: string): string => {
      const line = contract.split('\n').find((l) => l.startsWith(`| ${label}`))
      expect(line, label).toBeDefined()
      return line as string
    }
    const ticked = (line: string): string[] => [...new Set([...line.split('|')[2].matchAll(/`([a-z_]+)`/g)].map((m) => m[1]))]
    const schema = JSON.parse(readFileSync(join(schemaDir, 'candidate.schema.json'), 'utf8')).$defs
    expect(ticked(row('知识条目 `kind`'))).toEqual(schema.kind.enum)
    expect(ticked(row('候选状态（服务端）'))).toEqual(schema.status.enum)
    expect(ticked(row('候选退回原因 `return_reason`'))).toEqual(schema.return_reason.enum)
  })
})
