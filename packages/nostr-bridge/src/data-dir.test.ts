import { afterEach, describe, expect, it } from 'vitest'
import { CONFIG } from './config.js'

const keys = ['HOME', 'AGENT24_DATA_DIR', 'A24_NOSTR_PROFILE', 'A24_NOSTR_HEALTH_FILE', 'A24_NOSTR_IDENTITY'] as const
const saved = Object.fromEntries(keys.map((key) => [key, process.env[key]]))
afterEach(() => {
  for (const key of keys) {
    const value = saved[key]
    if (value === undefined) delete process.env[key]
    else process.env[key] = value
  }
})

describe('nostr data dir', () => {
  it('uses AGENT24_DATA_DIR as-is, otherwise $HOME/.agent24, without folding ..', () => {
    delete process.env.A24_NOSTR_PROFILE
    delete process.env.A24_NOSTR_HEALTH_FILE
    delete process.env.A24_NOSTR_IDENTITY
    process.env.AGENT24_DATA_DIR = '/a/link/../x'
    expect(CONFIG.DAEMON_STATE_FILE).toBe('/a/link/../x/daemon.json')
    expect(CONFIG.PROFILE_FILE).toBe('/a/link/../x/agent-profile.yml')
    expect(CONFIG.HEALTH_FILE).toBe('/a/link/../x/nostr-bridge-health-agent24.json')
    delete process.env.AGENT24_DATA_DIR
    process.env.HOME = '/a/link/..'
    expect(CONFIG.DAEMON_STATE_FILE).toBe('/a/link/../.agent24/daemon.json')
    expect(CONFIG.PROFILE_FILE).toBe('/a/link/../.agent24/agent-profile.yml')
    expect(CONFIG.HEALTH_FILE).toBe('/a/link/../.agent24/nostr-bridge-health-agent24.json')
  })
})
