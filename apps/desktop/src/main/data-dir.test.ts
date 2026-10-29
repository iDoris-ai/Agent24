import { afterEach, describe, expect, it } from 'vitest'
import * as path from 'node:path'
import { daemonStatePath } from './backend-manager'
import { defaultComponentsRoot } from './open-design-component'

describe('data-dir: open-design components root follows the data dir', () => {
  const saved = process.env['AGENT24_DATA_DIR']
  afterEach(() => {
    if (saved === undefined) delete process.env['AGENT24_DATA_DIR']
    else process.env['AGENT24_DATA_DIR'] = saved
  })

  it('defaultComponentsRoot() is <AGENT24_DATA_DIR>/components/open-design', () => {
    const dir = path.resolve(path.sep, 'tmp', 'a24-data-dir-test')
    process.env['AGENT24_DATA_DIR'] = dir
    expect(defaultComponentsRoot()).toBe(path.join(dir, 'components', 'open-design'))
  })
})

describe('data-dir: consumers join literally (no `..` folding)', () => {
  const saved = { HOME: process.env['HOME'], DIR: process.env['AGENT24_DATA_DIR'] }
  afterEach(() => {
    for (const [k, v] of [['HOME', saved.HOME], ['AGENT24_DATA_DIR', saved.DIR]] as const) {
      if (v === undefined) delete process.env[k]
      else process.env[k] = v
    }
  })

  it('daemonStatePath() with HOME=/a/link/.. keeps /../ in the path', () => {
    delete process.env['AGENT24_DATA_DIR']
    process.env['HOME'] = '/a/link/..'
    expect(daemonStatePath()).toBe(['/a/link/..', '.agent24', 'daemon.json'].join('/'))
    expect(daemonStatePath()).toContain('/../')
  })

  it('daemonStatePath() with AGENT24_DATA_DIR=/a/link/../x keeps /../ in the path', () => {
    process.env['AGENT24_DATA_DIR'] = '/a/link/../x'
    expect(daemonStatePath()).toBe('/a/link/../x/daemon.json')
  })

  it('defaultComponentsRoot() with HOME=/a/link/.. keeps /../ in the path', () => {
    delete process.env['AGENT24_DATA_DIR']
    process.env['HOME'] = '/a/link/..'
    expect(defaultComponentsRoot()).toBe('/a/link/../.agent24/components/open-design')
    expect(defaultComponentsRoot()).toContain('/../')
  })
})
