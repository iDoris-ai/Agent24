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

// One `key: value` pair. Key: 'quoted' (any characters, so `a-b` and `a:b` work), "quoted" or a bare word.
// Value: 'single', "double" or `backtick` quotes. Several pairs can share one line.
const ENTRY =
  /\s*(?:'((?:[^'\\]|\\.)*)'|"((?:[^"\\]|\\.)*)"|([\w.]+))\s*:\s*(?:'((?:[^'\\]|\\.)*)'|"((?:[^"\\]|\\.)*)"|`((?:[^`\\]|\\.)*)`)\s*,?/y

// Parse the body lines of a th block (not the `th: {` line or the closing brace). `rest` is whatever
// on a line could not be read as an entry; blank lines and `//` comments are fine.
function parseThaiBlock(source: string): { entries: Array<{ key: string; value: string }>; unparsed: Array<{ line: number; text: string }> } {
  const entries: Array<{ key: string; value: string }> = []
  const unparsed: Array<{ line: number; text: string }> = []
  const lines = thaiBlock(source).split('\n')
  const closed = lines.length > 1 && /^ {2}\}/.test(lines[lines.length - 1])
  for (let i = 1; i < (closed ? lines.length - 1 : lines.length); i++) {
    const text = lines[i]
    let pos = 0
    for (;;) {
      ENTRY.lastIndex = pos // sticky: a failed match resets lastIndex, so keep our own position
      const m = ENTRY.exec(text)
      if (!m) break
      entries.push({ key: m[1] ?? m[2] ?? m[3], value: m[4] ?? m[5] ?? m[6] })
      pos = ENTRY.lastIndex
    }
    const rest = text.slice(pos).trim()
    if (rest !== '' && !rest.startsWith('//')) unparsed.push({ line: i + 1, text: text.trim() })
  }
  return { entries, unparsed }
}

function thaiEntries(source: string): Array<{ key: string; value: string }> {
  return parseThaiBlock(source).entries
}

// Lines in a th block that look like entries but could not be read (line numbers count from the `th: {` line).
function thaiUnparsed(source: string): Array<{ line: number; text: string }> {
  return parseThaiBlock(source).unparsed
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
  // Round 2 (Arm): "schedule" is กำหนดการ, not ตารางเวลา. (กำหนดเวลา is a normal word for "deadline", so it is not banned.)
  // Round 2 (Arm, 24 Oct): the "please" word is โปรด, not กรุณา.
  { name: '"please" must be โปรด, not กรุณา', bad: /กรุณา/ },
  { name: '"schedule" must be กำหนดการ, not ตารางเวลา', bad: /ตารางเวลา/ },
  // Round 3 (Arm, from real Thai localizations, audit-2): memory feature, restart, ๆ, run, approval, stale.
  { name: 'memory feature must be ความทรงจำ, not ความจำ (หน่วยความจำ = RAM is fine)', bad: /(?<!หน่วย)(?<!ทรง)ความจำ(?!เป็น)/ },
  { name: '"restart" must be รีสตาร์ท, not รีสตาร์ต', bad: /รีสตาร์ต/ },
  { name: 'no whitespace before ๆ (write ทุกๆ, not ทุก ๆ)', bad: /\sๆ/ },
  { name: '"run" as a verb must be เรียกใช้, not รัน (รันไทม์ = runtime is fine)', bad: /(?:^|[\s“"'▶(]|กำลัง|ที่|เพื่อ|จะ|ให้|ได้|แล้ว)รัน(?!ไทม์)/ },
  { name: '"awaiting approval" must be รอการอนุมัติ, not รออนุมัติ', bad: /รออนุมัติ/ },
  { name: '"stale data" must be ล้าสมัย, not ไม่เป็นปัจจุบัน', bad: /ไม่เป็นปัจจุบัน/ },
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

  // The ban rules must not hit normal Thai words (answers review of #1440, Low L1).
  it('memory and run rules do not hit ความจำเป็น / การันตี, but hit the banned uses', () => {
    const rule = (part: string) => RULES.find((r) => r.name.includes(part))!.bad
    const memory = rule('memory feature')
    const run = rule('"run" as a verb')
    for (const ok of ['ความจำเป็น', 'หน่วยความจำ']) expect(memory.test(ok), ok).toBe(false)
    for (const bad of ['ความจำ', 'ค้นหาความจำ']) expect(memory.test(bad), bad).toBe(true)
    for (const ok of ['การันตี', 'รันไทม์']) expect(run.test(ok), ok).toBe(false)
    for (const bad of ['▶ รัน', 'กำลังรัน…', 'งานที่รัน', 'เพื่อรันโค้ด', 'รันทันที', 'ทุกครั้งที่รัน']) expect(run.test(bad), bad).toBe(true)
  })

  // The parser must not silently skip entries (OA-M1-07, answers review of #1175, Low 2).
  it('parser reads dash and colon keys and single, double and backtick values', () => {
    const src = [
      '  th: {',
      "    'nav.modules-manager': 'โมดูล', \"a:b\": \"ค่า\",",
      '    plain: `ข้อความ`,',
      "    // a comment",
      "    'it': 'it\\'s ok',",
      '  },',
    ].join('\n')
    expect(thaiEntries(src)).toEqual([
      { key: 'nav.modules-manager', value: 'โมดูล' },
      { key: 'a:b', value: 'ค่า' },
      { key: 'plain', value: 'ข้อความ' },
      { key: 'it', value: "it\\'s ok" },
    ])
    expect(thaiUnparsed(src)).toEqual([])
  })

  it('parser reports a th line it cannot read (so the gap cannot come back)', () => {
    const src = ['  th: {', "    'a': 'ok',", "    'b': 'first half'", "      + 'second half',", '    c: someVariable,', '  },'].join('\n')
    expect(thaiUnparsed(src).map((u) => u.line)).toEqual([4, 5])
  })

  it('reads every th entry in the real files, none left unparsed', () => {
    const left: string[] = []
    for (const file of files) {
      for (const u of thaiUnparsed(readFileSync(file, 'utf8'))) left.push(`${file.slice(RENDERER_DIR.length + 1)}:${u.line} ${u.text.slice(0, 60)}`)
    }
    expect(left).toEqual([])
    expect(thaiValue('App.i18n.ts', 'nav.modules-manager')).toBeTruthy()
    expect(thaiValue('pages/ModulesManager.i18n.ts', 'tier.third-party')).toBeTruthy()
  })

  it('App menu names match the page names', () => {
    expect(thaiValue('App.i18n.ts', 'nav.schedules')).toBe('กำหนดการ')
    expect(thaiValue('App.i18n.ts', 'title.schedules')).toBe('กำหนดการ')
    expect(thaiValue('App.i18n.ts', 'nav.workbench')).toBe('โต๊ะทำงาน')
    expect(thaiValue('App.i18n.ts', 'nav.workbench')).toBe(thaiValue('pages/Workbench.i18n.ts', 'title'))
  })
})
