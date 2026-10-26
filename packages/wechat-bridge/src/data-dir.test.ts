import { afterEach, describe, expect, it } from 'vitest'
import { CONFIG } from './config.js'

const saved = { HOME: process.env.HOME, DIR: process.env.AGENT24_DATA_DIR }
afterEach(() => {
  if (saved.HOME === undefined) delete process.env.HOME
  else process.env.HOME = saved.HOME
  if (saved.DIR === undefined) delete process.env.AGENT24_DATA_DIR
  else process.env.AGENT24_DATA_DIR = saved.DIR
})

describe('wechat data dir', () => {
  it('uses AGENT24_DATA_DIR as-is, otherwise $HOME/.agent24, without folding ..', () => {
    process.env.AGENT24_DATA_DIR = '/a/link/../x'
    expect(CONFIG.DAEMON_STATE_FILE).toBe('/a/link/../x/daemon.json')
    expect(CONFIG.TOKEN_FILE).toBe('/a/link/../x/wechat-token.json')
    delete process.env.AGENT24_DATA_DIR
    process.env.HOME = '/a/link/..'
    expect(CONFIG.DAEMON_STATE_FILE).toBe('/a/link/../.agent24/daemon.json')
    expect(CONFIG.TOKEN_FILE).toBe('/a/link/../.agent24/wechat-token.json')
  })

  it('keeps /../ in both the session path and the token path when AGENT24_DATA_DIR is /a/link/..', () => {
    process.env.AGENT24_DATA_DIR = '/a/link/..'
    expect(CONFIG.TOKEN_FILE).toBe('/a/link/../wechat-token.json')
    expect(CONFIG.SESSION_FILE).toBe('/a/link/../wechat-sessions.json')
    expect(CONFIG.TOKEN_FILE).toContain('/../')
    expect(CONFIG.SESSION_FILE).toContain('/../')
  })
})
