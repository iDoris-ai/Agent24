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

// Every `key: 'value'` pair in a th block (key may be quoted; several pairs can share one line).
function thaiEntries(source: string): Array<{ key: string; value: string }> {
  const out: Array<{ key: string; value: string }> = []
  for (const m of thaiBlock(source).matchAll(/'?([\w.]+)'?:\s*'((?:[^'\\]|\\.)*)'/g)) out.push({ key: m[1], value: m[2] })
  return out
}

function thaiValue(file: string, key: string): string | undefined {
  return thaiEntries(readFileSync(join(RENDERER_DIR, file), 'utf8')).find((e) => e.key === key)?.value
}

const RULES: Array<{ name: string; bad: RegExp }> = [
  { name: '"chat" must be แชท, not แชต', bad: /แชต/ },
  { name: '"token" must be โทเค็น, not โทเคน', bad: /โทเคน/ },
  { name: 'sara am must be U+0E33, not U+0E4D U+0E32', bad: /ํา/ },
  // Round 2 (Arm): "failed" is ไม่สำเร็จ, not ล้มเหลว.
  { name: '"failed" must be ไม่สำเร็จ, not ล้มเหลว', bad: /ล้มเหลว/ },
  // Round 2 (Arm): the background service is บริการเบื้องหลัง; no เดมอน, no English "daemon" in Thai text.
  { name: '"daemon" must be บริการเบื้องหลัง, not เดมอน', bad: /เดมอน/ },
  { name: 'no English word "daemon" in Thai text', bad: /daemon/i },
  // Round 2 (Arm): "schedule" is กำหนดการ, not ตารางเวลา or กำหนดเวลา. No exemptions.
  // Round 2 (Arm, 24 Oct): the "please" word is โปรด, not กรุณา.
  { name: '"please" must be โปรด, not กรุณา', bad: /กรุณา/ },
  { name: '"schedule" must be กำหนดการ, not ตารางเวลา', bad: /ตารางเวลา/ },
  { name: '"schedule" must be กำหนดการ, not กำหนดเวลา', bad: /กำหนดเวลา/ },
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
        const name = file.slice(RENDERER_DIR.length + 1)
        for (const { key, value } of thaiEntries(readFileSync(file, 'utf8'))) {
          if (rule.bad.test(value)) hits.push(`${name} ${key}: ${value.slice(0, 80)}`)
        }
      }
      expect(hits).toEqual([])
    })
  }

  it('App menu names match the page names', () => {
    expect(thaiValue('App.i18n.ts', 'nav.schedules')).toBe('กำหนดการ')
    expect(thaiValue('App.i18n.ts', 'title.schedules')).toBe('กำหนดการ')
    expect(thaiValue('App.i18n.ts', 'nav.workbench')).toBe('โต๊ะทำงาน')
    expect(thaiValue('App.i18n.ts', 'nav.workbench')).toBe(thaiValue('pages/Workbench.i18n.ts', 'title'))
  })
})
