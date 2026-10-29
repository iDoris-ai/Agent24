import { readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import {
  CLIENT_PROTOCOL_VERSION,
  protocolCompatibility,
} from './protocol-compatibility'

interface CompatibilityCase {
  name: string
  daemon: number | null
  client: number
  compatible: boolean
  reason?: string
}

interface CompatibilityFixture {
  client_protocol_version: number
  cases: CompatibilityCase[]
}

const fixture = JSON.parse(
  readFileSync(
    path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../../protocol/fixtures/protocol-compatibility.json'),
    'utf8',
  ),
) as CompatibilityFixture

describe('protocol compatibility (shared fixture)', () => {
  it('speaks the fixture client version', () => {
    expect(CLIENT_PROTOCOL_VERSION).toBe(fixture.client_protocol_version)
  })

  it.each(fixture.cases)('$name', (entry) => {
    const got = protocolCompatibility(entry.daemon, entry.client)
    if (entry.compatible) {
      expect(got).toEqual({ compatible: true })
    } else {
      expect(got).toEqual({ compatible: false, reason: entry.reason })
    }
  })
})
