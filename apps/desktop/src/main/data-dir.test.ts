import { describe, expect, it } from 'vitest'
import * as fs from 'node:fs'
import * as path from 'node:path'
import { fileURLToPath } from 'node:url'
import { DataDirError, resolveDataDir, type DataDirEnv, type DataDirPlatform } from './data-dir'

interface Case {
  name: string
  platform: DataDirPlatform
  env: DataDirEnv
  expect: { path?: string; error?: string }
}

const fixturePath = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../../../protocol/fixtures/data-dir/cases.json',
)
const cases = (JSON.parse(fs.readFileSync(fixturePath, 'utf8')) as { cases: Case[] }).cases

describe('data-dir: shared cross-language fixture', () => {
  it('fixture is non-trivial', () => {
    expect(cases.length).toBeGreaterThanOrEqual(20)
  })

  for (const c of cases) {
    it(c.name, () => {
      if (c.expect.error !== undefined) {
        let thrown: unknown
        try {
          resolveDataDir(c.platform, c.env)
        } catch (error) {
          thrown = error
        }
        expect(thrown).toBeInstanceOf(DataDirError)
        expect((thrown as DataDirError).code).toBe(c.expect.error)
      } else {
        expect(resolveDataDir(c.platform, c.env)).toBe(c.expect.path)
      }
    })
  }
})

describe('data-dir: regression', () => {
  it('default path is unchanged for mac and linux (~/.agent24)', () => {
    expect(resolveDataDir('macos', { HOME: '/Users/u' })).toBe('/Users/u/.agent24')
    expect(resolveDataDir('linux', { HOME: '/home/u' })).toBe('/home/u/.agent24')
  })
})
