// M2-02: check a serialized DomainOsList-shaped response against the public
// OpenAPI schema, including the registry error and missing-resource case.

import { beforeAll, describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import Ajv2020 from 'ajv/dist/2020.js'
import { parse } from 'yaml'

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')
let sample: unknown
let attachedSample: unknown
let validate: ((doc: unknown) => boolean) & { errors?: unknown } = Object.assign(() => false, {})
let validateAttached: ((doc: unknown) => boolean) & { errors?: unknown } = Object.assign(() => false, {})

beforeAll(() => {
  const openapi = parse(readFileSync(join(repoRoot, 'protocol', 'openapi.yaml'), 'utf8')) as Record<string, any>
  sample = JSON.parse(readFileSync(join(repoRoot, 'protocol', 'fixtures', 'os', 'list.json'), 'utf8')) as unknown
  attachedSample = JSON.parse(readFileSync(join(repoRoot, 'protocol', 'fixtures', 'attached', 'list.json'), 'utf8')) as unknown
  const ajv = new Ajv2020.default({ strict: false, allErrors: true })
  validate = ajv.compile({ ...openapi, $ref: '#/components/schemas/DomainOsList' }) as typeof validate
  validateAttached = ajv.compile({ ...openapi, $ref: '#/components/schemas/AttachedList' }) as typeof validateAttached
})

describe('DomainOsList OpenAPI schema', () => {
  it('accepts the representative daemon response with registry_error and missing resources', () => {
    expect(validate(sample), JSON.stringify(validate.errors)).toBe(true)
  })

  it('rejects a response missing the required resources field', () => {
    const malformed = JSON.parse(JSON.stringify(sample)) as { modules: Array<Record<string, unknown>> }
    delete malformed.modules[0].resources
    expect(validate(malformed)).toBe(false)
  })

  it('accepts the list_attached response shape and rejects token leakage', () => {
    expect(validateAttached(attachedSample), JSON.stringify(validateAttached.errors)).toBe(true)
    const doc = attachedSample as { modules: Array<Record<string, unknown>> }
    for (const field of ['token', 'token_sha256']) expect(validateAttached({ ...doc, modules: [{ ...doc.modules[0], [field]: 'secret' }] })).toBe(false)
  })
})
