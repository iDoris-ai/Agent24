import { describe, it, expect } from 'vitest'
import { deriveStatus } from './backend-manager'

describe('deriveStatus (F1b tray daemon status)', () => {
  it('a user-initiated stop pins `stopped`, even if a stale endpoint lingers', () => {
    expect(deriveStatus({ userStopped: true, hasEndpoint: false, healthy: false })).toBe('stopped')
    // must NOT read as running/starting — a manual stop is authoritative
    expect(deriveStatus({ userStopped: true, hasEndpoint: true, healthy: true })).toBe('stopped')
  })

  it('a healthy endpoint is `running`', () => {
    expect(deriveStatus({ userStopped: false, hasEndpoint: true, healthy: true })).toBe('running')
  })

  it('spawned-but-not-ready (endpoint present, not yet healthy) is `starting`', () => {
    expect(deriveStatus({ userStopped: false, hasEndpoint: true, healthy: false })).toBe('starting')
  })

  it('no endpoint yet (or briefly between respawns), not user-stopped, is `starting`', () => {
    expect(deriveStatus({ userStopped: false, hasEndpoint: false, healthy: false })).toBe('starting')
  })
})
