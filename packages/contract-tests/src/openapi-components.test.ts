// C9-D: GET /components host view carries the mount's kernel grants. The redacted
// view stays id, version, state and provides. Denial codes are a closed enum.

import { beforeAll, describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import Ajv2020 from 'ajv/dist/2020.js'
import { parse } from 'yaml'

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')
const CODES = ['not_declared', 'above_ceiling', 'no_channel', 'requires_invalid']

let openapi: Record<string, any>
let validateList: ((doc: unknown) => boolean) & { errors?: unknown }
let validateView: ((doc: unknown) => boolean) & { errors?: unknown }
let validateRedacted: ((doc: unknown) => boolean) & { errors?: unknown }

const hostRow = {
  id: 'hyphae',
  version: 'v0.30.1',
  state: 'absent',
  form: 'resident',
  provides: [{ id: 'nostr-dm' }],
  kernel_grants: [] as string[],
  declared: { kernel: ['events'], operations: [] as string[] },
  denied: [{ item: 'events', code: 'no_channel' }],
}

const redactedRow = {
  id: 'hyphae',
  version: null,
  state: 'absent',
  provides: [{ id: 'nostr-dm' }],
}

beforeAll(() => {
  openapi = parse(readFileSync(join(repoRoot, 'protocol', 'openapi.yaml'), 'utf8')) as Record<string, any>
  const ajv = new Ajv2020.default({ strict: false, allErrors: true })
  validateList = ajv.compile({ ...openapi, $ref: '#/components/schemas/ComponentList' }) as typeof validateList
  validateView = ajv.compile({ ...openapi, $ref: '#/components/schemas/ComponentView' }) as typeof validateView
  validateRedacted = ajv.compile({ ...openapi, $ref: '#/components/schemas/ComponentRedactedView' }) as typeof validateRedacted
})

describe('ComponentList kernel grants', () => {
  it('accepts a host row with mount grants and a redacted row without denials', () => {
    const mounted = {
      ...hostRow,
      id: 'modelnodeps',
      state: 'ready',
      kernel_grants: ['events'],
      declared: { kernel: ['events', 'models'], operations: [] },
      denied: [
        { item: 'models', code: 'no_channel' },
        { item: 'memory', code: 'not_declared' },
      ],
    }
    expect(validateList({ components: [hostRow, mounted, redactedRow] }), JSON.stringify(validateList.errors)).toBe(true)
    expect(validateView(hostRow), JSON.stringify(validateView.errors)).toBe(true)
    expect(validateRedacted(redactedRow), JSON.stringify(validateRedacted.errors)).toBe(true)
  })

  it('rejects a host row that omits kernel_grants and a denial code outside the closed set', () => {
    const { kernel_grants: _dropped, ...withoutGrants } = hostRow
    expect(validateView(withoutGrants)).toBe(false)
    expect(validateView({ ...hostRow, denied: [{ item: 'events', code: 'resolver_said_so' }] })).toBe(false)
    expect(openapi.components.schemas.ComponentDenial.properties.code.enum).toEqual(CODES)
    const redactedProps = Object.keys(openapi.components.schemas.ComponentRedactedView.properties)
    expect(redactedProps).not.toContain('denied')
    expect(redactedProps).not.toContain('kernel_grants')
  })
})
