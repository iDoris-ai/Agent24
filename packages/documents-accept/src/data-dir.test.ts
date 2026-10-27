import { afterEach, describe, expect, it } from 'vitest'
import { defaultDaemonStateFile } from './client'

const saved = { HOME: process.env.HOME, DIR: process.env.AGENT24_DATA_DIR }
afterEach(() => {
  if (saved.HOME === undefined) delete process.env.HOME
  else process.env.HOME = saved.HOME
  if (saved.DIR === undefined) delete process.env.AGENT24_DATA_DIR
  else process.env.AGENT24_DATA_DIR = saved.DIR
})

describe('default daemon state file', () => {
  it('uses AGENT24_DATA_DIR as-is, otherwise $HOME/.agent24, without folding ..', () => {
    process.env.AGENT24_DATA_DIR = '/a/link/../x'
    expect(defaultDaemonStateFile()).toBe('/a/link/../x/daemon.json')
    delete process.env.AGENT24_DATA_DIR
    process.env.HOME = '/a/link/..'
    expect(defaultDaemonStateFile()).toBe('/a/link/../.agent24/daemon.json')
  })
})
