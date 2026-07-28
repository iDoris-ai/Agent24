import { describe, it, expect } from 'vitest'
import fs from 'node:fs'
import path from 'node:path'
import { validateManifest, consentSummary, REQUIRED_FIELDS, ID_PATTERN } from './module-manifest'

function valid(): Record<string, unknown> {
  return {
    id: '@acme/weather',
    version: '0.1.0',
    name: 'Weather',
    description: 'Fetches the weather',
    type: 'headless',
    permissions: ['network'],
  }
}

describe('validateManifest', () => {
  it('accepts a well-formed manifest', () => {
    expect(validateManifest(valid())).toEqual([])
  })

  it('rejects a non-object', () => {
    expect(validateManifest(null).length).toBeGreaterThan(0)
    expect(validateManifest('nope').length).toBeGreaterThan(0)
    expect(validateManifest([]).length).toBeGreaterThan(0)
  })

  it('flags every missing required field', () => {
    for (const field of REQUIRED_FIELDS) {
      const m = valid()
      delete m[field]
      const errors = validateManifest(m)
      expect(errors.some((e) => e.includes(field))).toBe(true)
    }
  })

  it('rejects a non-npm id', () => {
    const m = valid()
    m.id = 'Not A Package Name!'
    expect(validateManifest(m).some((e) => e.includes('valid module id'))).toBe(true)
  })

  it('requires permissions to be an array of strings', () => {
    const bad = valid()
    bad.permissions = 'network'
    expect(validateManifest(bad).some((e) => e.includes('array'))).toBe(true)

    const badItems = valid()
    badItems.permissions = ['network', 42]
    expect(validateManifest(badItems).some((e) => e.includes('string'))).toBe(true)
  })

  it('does NOT reject unknown enum values or extra fields (open sets, forward-compat)', () => {
    const m = valid()
    m.type = 'some-future-kind' // schema enum is an OPEN set per its own note
    m.permissions = ['some-future-permission']
    ;(m as Record<string, unknown>).futureField = { anything: true }
    expect(validateManifest(m)).toEqual([])
  })

  // Drift guard: the validator's hard-coded rules must stay in sync with the
  // authority (protocol/module.schema.json). If the schema changes its required
  // fields or id pattern, this fails until the validator is updated.
  it('stays in sync with module.schema.json', () => {
    const schemaPath = path.resolve(__dirname, '../../../protocol/module.schema.json')
    const schema = JSON.parse(fs.readFileSync(schemaPath, 'utf8')) as {
      required: string[]
      properties: { id: { pattern: string } }
    }
    expect([...REQUIRED_FIELDS].sort()).toEqual([...schema.required].sort())
    // RegExp.source escapes the forward slash (`\/`); the schema JSON does not.
    // Normalize before comparing — they are otherwise the same pattern.
    expect(ID_PATTERN.source.replaceAll('\\/', '/')).toBe(schema.properties.id.pattern)
  })
})

describe('consentSummary (H10)', () => {
  it('extracts what the module declares it wants', () => {
    expect(consentSummary(valid())).toEqual({
      id: '@acme/weather',
      name: 'Weather',
      type: 'headless',
      permissions: ['network'],
    })
  })

  it('falls back to the id when there is no display name', () => {
    const m = valid()
    delete m.name
    expect(consentSummary(m).name).toBe('@acme/weather')
  })

  it('drops non-string permission entries defensively', () => {
    const m = valid()
    m.permissions = ['network', 42, 'filesystem']
    expect(consentSummary(m).permissions).toEqual(['network', 'filesystem'])
  })
})
