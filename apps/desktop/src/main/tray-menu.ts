// The tray's text, built from the daemon status and the current UI language.
// Pure (no Electron): main.ts feeds it to Tray/Menu, tests read it directly.

import { mt } from './main-i18n'
import type { CliToolStatus } from '../shared/ipc-types'

export type TrayStatus = 'running' | 'starting' | 'stopped' | 'error' | 'blocked'

const TITLES: Record<TrayStatus, string> = {
  running: '⚡A24', starting: '…A24', stopped: '⚠️A24', error: '⚠️A24', blocked: '⚠️A24',
}

export interface TrayInput {
  status: TrayStatus
  external: boolean
  externalPid: number | null
  foreignPid: number | null
  backendKind: string
  cliTool?: Pick<CliToolStatus, 'state' | 'version'>
}

export type TrayAction = 'start' | 'disconnect' | 'restart' | 'stop' | 'show' | 'quit' | 'cli-settings'
export type TrayItem = { type: 'separator' } | { label: string; enabled: false } | { label: string; action: TrayAction }

export interface TrayView { title: string; tooltip: string; items: TrayItem[] }

export function buildTrayView(input: TrayInput): TrayView {
  const cliItems: TrayItem[] = []
  const cli = input.cliTool
  if (cli && (cli.state === 'missing' || cli.state === 'outdated' || cli.state === 'unknown')) {
    cliItems.push(
      { label: mt(`cli.${cli.state}`, { version: cli.version ?? '?' }), action: 'cli-settings' },
      { type: 'separator' },
    )
  }
  const base = mt(`tray.${input.status}`)
  // DEP-A5: an external daemon gets a visibly different label so the user can
  // tell it apart before hitting stop.
  const label = input.external
    ? mt('tray.external', { label: base, pid: input.externalPid ?? '?' })
    : input.foreignPid !== null ? mt('tray.foreign', { label: base, pid: input.foreignPid }) : base
  const tooltip = `Agent24 — ${label.replace(/^[●○✕]\s*/, '')} (${input.backendKind})`
  // DEP-A5 #2: stop/restart only ever act on a daemon this app spawned, so the
  // external case is worded "disconnect", never implying a kill. An
  // incompatible daemon is neither reused nor killed: no start/stop at all.
  const daemonItems: TrayItem[] =
    input.status === 'error'
      ? [{ label: mt('tray.errorHint'), enabled: false }]
      : input.status === 'stopped'
        ? [{ label: mt('tray.start'), action: 'start' }]
        : input.external
          ? [{ label: mt('tray.disconnect'), action: 'disconnect' }]
          : [{ label: mt('tray.restart'), action: 'restart' }, { label: mt('tray.stop'), action: 'stop' }]
  return {
    title: TITLES[input.status],
    tooltip,
    items: [
      ...cliItems,
      { label, enabled: false },
      { type: 'separator' },
      { label: mt('tray.show'), action: 'show' },
      { type: 'separator' },
      ...daemonItems,
      { type: 'separator' },
      { label: mt('tray.quit'), action: 'quit' },
    ],
  }
}
