import { describe, expect, it } from 'vitest'
import * as fs from 'node:fs'
import * as path from 'node:path'
import { fileURLToPath } from 'node:url'
import { DataDirError, joinLiteralFor, resolveDataDir, type DataDirEnv, type DataDirPlatform } from './index'

interface Case {
  name: string
  platform: DataDirPlatform
  env: DataDirEnv
  expect: { path?: string; error?: string }
}

const fixturePath = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../../protocol/fixtures/data-dir/cases.json',
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

describe('joinLiteralFor', () => {
  it('uses the target platform separator and never folds', () => {
    expect(joinLiteralFor('linux', '/a/link/../', 'x', 'y')).toBe('/a/link/../x/y')
    expect(joinLiteralFor('macos', '/a/./b', 'x')).toBe('/a/./b/x')
    expect(joinLiteralFor('windows', 'C:\\a\\..\\', 'x', 'y')).toBe('C:\\a\\..\\x\\y')
  })
})
