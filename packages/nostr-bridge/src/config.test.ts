import { describe, it, expect } from 'vitest'
import { parseF4bInboundEnabled } from './config.js'

// COMM-5a: the structural guarantee in inbound.ts (pollOnce defaults to
// frozen) is only as good as the config mapping that decides what `main.ts`
// passes it. `CONFIG.F4B_INBOUND_ENABLED` reads `process.env` once at module
// load, so it can't be exercised directly with different env values in one
// test run — `parseF4bInboundEnabled` is the pure function pulled out of that
// object literal so this mapping itself has a regression test.
describe('parseF4bInboundEnabled (COMM-5a F4b freeze config)', () => {
  it('A24_NOSTR_F4B_INBOUND unset ⇒ false (frozen, the production default)', () => {
    expect(parseF4bInboundEnabled(undefined)).toBe(false)
  })

  it("A24_NOSTR_F4B_INBOUND='1' ⇒ true (legacy dispatch restored)", () => {
    expect(parseF4bInboundEnabled('1')).toBe(true)
  })

  it('any other value (empty string, "0", "true", garbage) ⇒ false — fail closed', () => {
    expect(parseF4bInboundEnabled('')).toBe(false)
    expect(parseF4bInboundEnabled('0')).toBe(false)
    expect(parseF4bInboundEnabled('true')).toBe(false)
    expect(parseF4bInboundEnabled('yes')).toBe(false)
  })
})
