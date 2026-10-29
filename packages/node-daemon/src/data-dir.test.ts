import { afterEach, describe, expect, it } from 'vitest'
import { modulesDir } from './module-installer'
import { moduleStateFile } from './module-state'

const saved = { HOME: process.env.HOME, DIR: process.env.AGENT24_DATA_DIR }
afterEach(() => {
  if (saved.HOME === undefined) delete process.env.HOME
  else process.env.HOME = saved.HOME
  if (saved.DIR === undefined) delete process.env.AGENT24_DATA_DIR
  else process.env.AGENT24_DATA_DIR = saved.DIR
})

describe('node-daemon data dir', () => {
  it('uses AGENT24_DATA_DIR as-is, otherwise $HOME/.agent24, without folding ..', () => {
    process.env.AGENT24_DATA_DIR = '/a/link/../x'
    expect(modulesDir()).toBe('/a/link/../x/modules')
    expect(moduleStateFile()).toBe('/a/link/../x/module-state.json')
    delete process.env.AGENT24_DATA_DIR
    process.env.HOME = '/a/link/..'
    expect(modulesDir()).toBe('/a/link/../.agent24/modules')
    expect(moduleStateFile()).toBe('/a/link/../.agent24/module-state.json')
  })
})
