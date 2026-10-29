import { afterEach, beforeEach, expect, it } from 'vitest'
import { onMainLocaleChange, resetMainLocaleForTest, setMainLocale } from './main-i18n'
import { buildTrayView, type TrayInput } from './tray-menu'

const input: TrayInput = {
  status: 'running', external: false, externalPid: null, foreignPid: null, backendKind: 'rust',
}
const outdated = { ...input, cliTool: { state: 'outdated', version: '0.3.0' } } as const

beforeEach(resetMainLocaleForTest)
afterEach(resetMainLocaleForTest)

it('shows the CLI update action in English by default', () => {
  expect(buildTrayView(outdated).items.slice(0, 2)).toEqual([
    { label: 'Command line tool is outdated (0.3.0) — Update', action: 'cli-settings' },
    { type: 'separator' },
  ])
})

it('updates CLI text when switching to Chinese and Thai', () => {
  setMainLocale('zh')
  expect(buildTrayView(outdated).items[0]).toEqual({
    label: '命令行工具已过期（0.3.0）— 更新', action: 'cli-settings',
  })
  setMainLocale('th')
  expect(buildTrayView(outdated).items[0]).toEqual({
    label: 'เครื่องมือบรรทัดคำสั่งล้าสมัย (0.3.0) — อัปเดต', action: 'cli-settings',
  })
})

it.each([
  ['missing', 'Command line tool is missing — Install'],
  ['unknown', 'Could not read the CLI version on PATH — Install / Update'],
] as const)('offers CLI settings when the CLI is %s', (state, label) => {
  expect(buildTrayView({ ...input, cliTool: { state, version: null } }).items[0]).toEqual({
    label, action: 'cli-settings',
  })
})

it.each(['current', 'newer', 'unavailable'] as const)('hides the CLI action when the CLI is %s', (state) => {
  expect(buildTrayView({ ...input, cliTool: { state, version: '0.6.0' } })).toEqual(buildTrayView(input))
})

it('offers no start/stop for an incompatible daemon, only a localized hint', () => {
  const view = buildTrayView({ ...input, status: 'error' })
  expect(view.title).toBe('⚠️A24')
  expect(view.items).toContainEqual({ label: '✕ daemon: incompatible protocol', enabled: false })
  expect(view.items.some((i) => 'action' in i && ['start', 'stop', 'restart', 'disconnect'].includes(i.action))).toBe(false)
  setMainLocale('zh')
  expect(buildTrayView({ ...input, status: 'error' }).items).toContainEqual({ label: '✕ daemon：协议不兼容', enabled: false })
})

it.each([
  ['en', 'Protocol 2 (app 1) · pid 4242', 'recorded protocol 3', 'will not stop it'],
  ['zh', '协议 2（应用 1）· pid 4242', '记录的协议版本为 3', '不会终止它'],
  ['th', 'โปรโตคอล 2 (แอป 1) · pid 4242', 'โปรโตคอลที่บันทึกไว้ 3', 'จะไม่หยุด'],
] as const)('renders protocol mismatch details in %s', (locale, short, recorded, safety) => {
  setMainLocale(locale)
  const view = buildTrayView({
    ...input, status: 'error',
    detail: { kind: 'protocol_mismatch', daemonVersion: 2, clientVersion: 1, recordedVersion: 3, pid: 4242 },
  })
  expect(view.items).toContainEqual({ label: short, enabled: false })
  expect(view.tooltip).toContain(short)
  expect(view.tooltip).toContain(recorded)
  expect(view.tooltip).toContain(safety)
  if (locale !== 'zh') expect(view.tooltip).not.toMatch(/[一-鿿]/)
  expect(view.items.some((i) => 'action' in i && ['start', 'stop', 'restart', 'disconnect'].includes(i.action))).toBe(false)
})

it('refreshes missing protocol details on locale changes without a new backend status', () => {
  const error: TrayInput = {
    ...input, status: 'error',
    detail: { kind: 'protocol_mismatch', daemonVersion: null, clientVersion: 1, pid: 4242 },
  }
  let view = buildTrayView(error)
  const unsubscribe = onMainLocaleChange(() => { view = buildTrayView(error) })
  expect(view.tooltip).toContain('Protocol unknown (app 1) · pid 4242')
  expect(view.tooltip).toContain('daemon.json has no protocol version')
  setMainLocale('zh')
  expect(view.items).toContainEqual({ label: '协议 未知（应用 1）· pid 4242', enabled: false })
  expect(view.tooltip).toContain('daemon.json 没有记录协议版本')
  setMainLocale('th')
  expect(view.tooltip).toContain('โปรโตคอล ไม่ทราบ (แอป 1) · pid 4242')
  expect(view.tooltip).toContain('daemon.json ไม่ได้บันทึกเวอร์ชันโปรโตคอล')
  setMainLocale('en')
  expect(view.tooltip).toContain('Protocol unknown (app 1) · pid 4242')
  unsubscribe()
})
