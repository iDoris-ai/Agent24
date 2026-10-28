import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { getMainLocale, initMainLocale, mt, onMainLocaleChange, resetMainLocaleForTest, setMainLocale } from './main-i18n'
import { buildTrayView, type TrayInput } from './tray-menu'

const input = (over: Partial<TrayInput> = {}): TrayInput => ({
  status: 'running', external: false, externalPid: null, foreignPid: null, backendKind: 'rust', ...over,
})
const labels = (view: ReturnType<typeof buildTrayView>) =>
  view.items.flatMap((i) => ('label' in i ? [i.label] : []))

let dir: string
beforeEach(() => { dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-main-i18n-')); resetMainLocaleForTest() })
afterEach(() => { fs.rmSync(dir, { recursive: true, force: true }); resetMainLocaleForTest(); vi.unstubAllEnvs() })

describe('main-process i18n', () => {
  it('defaults to English, even when the system locale is Chinese', () => {
    vi.stubEnv('LANG', 'zh_CN.UTF-8')
    vi.stubEnv('LC_ALL', 'zh_CN.UTF-8')
    initMainLocale(dir)
    expect(getMainLocale()).toBe('en')
    expect(labels(buildTrayView(input()))).toEqual(['● daemon: running', 'Show window', 'Restart daemon', 'Stop daemon', 'Quit Agent24'])
    expect(mt('dialog.confirm')).toBe('Confirm')
  })

  it('tray labels follow a switch to Chinese and to Thai', () => {
    initMainLocale(dir)
    setMainLocale('zh')
    expect(labels(buildTrayView(input({ status: 'stopped' })))).toEqual(['✕ daemon：已停止', '显示窗口', '启动 daemon', '退出 Agent24'])
    setMainLocale('th')
    const th = labels(buildTrayView(input({ status: 'stopped' })))
    expect(th).toContain('แสดงหน้าต่าง')
    expect(th).toContain('ออกจาก Agent24')
    expect(th.join('')).not.toMatch(/[一-鿿]/)
    setMainLocale('en')
    expect(labels(buildTrayView(input({ status: 'stopped' })))).toContain('Start daemon')
  })

  it('external and foreign daemons are labelled in every language', () => {
    initMainLocale(dir)
    expect(labels(buildTrayView(input({ external: true, externalPid: 42 })))).toEqual([
      '● daemon: running (external daemon · pid 42)', 'Show window', 'Disconnect external daemon (does not stop it)', 'Quit Agent24',
    ])
    setMainLocale('zh')
    expect(buildTrayView(input({ external: true, externalPid: 42 })).items[0]).toMatchObject({ label: '● daemon：运行中（外部 daemon · pid 42）' })
    expect(buildTrayView(input({ foreignPid: 7 })).tooltip).toContain('pid 7')
  })

  it('notifies listeners (so the tray is rebuilt) only when the language really changes', () => {
    initMainLocale(dir)
    const seen = vi.fn()
    onMainLocaleChange(seen)
    setMainLocale('en')
    expect(seen).not.toHaveBeenCalled()
    setMainLocale('zh')
    setMainLocale('zh')
    setMainLocale('xx' as never)
    expect(seen.mock.calls).toEqual([['zh']])
  })

  it('remembers the choice across restarts; garbage on disk falls back to English', () => {
    initMainLocale(dir)
    setMainLocale('th')
    resetMainLocaleForTest()
    initMainLocale(dir)
    expect(getMainLocale()).toBe('th')
    fs.writeFileSync(path.join(dir, 'ui-locale.json'), '{"locale":"klingon"}')
    initMainLocale(dir)
    expect(getMainLocale()).toBe('en')
    fs.writeFileSync(path.join(dir, 'ui-locale.json'), 'not json')
    initMainLocale(dir)
    expect(getMainLocale()).toBe('en')
  })

  it('every language carries every key (no blank labels)', () => {
    initMainLocale(dir)
    for (const l of ['en', 'zh', 'th'] as const) {
      setMainLocale(l)
      for (const status of ['running', 'starting', 'stopped', 'blocked'] as const) {
        expect(labels(buildTrayView(input({ status }))).every((x) => x.length > 0)).toBe(true)
      }
    }
  })
})
