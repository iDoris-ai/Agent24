import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'

// Thai copy rules from the native-speaker review (Arm). Applied to every `th: { ... }` block
// in renderer/**/*.i18n.ts. en/zh blocks are not checked.
const RENDERER_DIR = join(__dirname, '..')

function listI18nFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const full = join(dir, name)
    if (statSync(full).isDirectory()) return listI18nFiles(full)
    return name.endsWith('.i18n.ts') ? [full] : []
  })
}

function thaiBlock(source: string): string {
  const lines = source.split('\n')
  const start = lines.findIndex((l) => /^ {2}th: \{/.test(l))
  if (start < 0) return ''
  const end = lines.findIndex((l, i) => i > start && /^ {2}\}/.test(l))
  return lines.slice(start, end < 0 ? undefined : end + 1).join('\n')
}

const RULES: Array<{ name: string; bad: RegExp }> = [
  { name: '"chat" must be แชท, not แชต', bad: /แชต/ },
  { name: '"token" must be โทเค็น, not โทเคน', bad: /โทเคน/ },
  { name: 'sara am must be U+0E33, not U+0E4D U+0E32', bad: /ํา/ },
]

describe('Thai UI copy', () => {
  const files = listI18nFiles(RENDERER_DIR)

  it('finds the th blocks', () => {
    expect(files.length).toBeGreaterThan(0)
    expect(files.filter((f) => thaiBlock(readFileSync(f, 'utf8')) !== '').length).toBeGreaterThan(0)
  })

  for (const rule of RULES) {
    it(rule.name, () => {
      const hits: string[] = []
      for (const file of files) {
        thaiBlock(readFileSync(file, 'utf8'))
          .split('\n')
          .forEach((line) => {
            if (rule.bad.test(line)) hits.push(`${file.slice(RENDERER_DIR.length + 1)}: ${line.trim().slice(0, 80)}`)
          })
      }
      expect(hits).toEqual([])
    })
  }
})
