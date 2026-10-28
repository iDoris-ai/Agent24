import { afterEach, beforeEach, expect, it } from 'vitest'
import { resetMainLocaleForTest, setMainLocale } from './main-i18n'
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
