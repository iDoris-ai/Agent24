// The tray's text, built from the daemon status and the current UI language.
// Pure (no Electron): main.ts feeds it to Tray/Menu, tests read it directly.

import { mt } from './main-i18n'

export type TrayStatus = 'running' | 'starting' | 'stopped' | 'blocked'

const TITLES: Record<TrayStatus, string> = {
  running: '⚡A24', starting: '…A24', stopped: '⚠️A24', blocked: '⚠️A24',
}

export interface TrayInput {
  status: TrayStatus
  external: boolean
  externalPid: number | null
  foreignPid: number | null
  backendKind: string
}

export type TrayAction = 'start' | 'disconnect' | 'restart' | 'stop' | 'show' | 'quit'
export type TrayItem = { type: 'separator' } | { label: string; enabled: false } | { label: string; action: TrayAction }

export interface TrayView { title: string; tooltip: string; items: TrayItem[] }

export function buildTrayView(input: TrayInput): TrayView {
  const base = mt(`tray.${input.status}`)
  // DEP-A5: an external daemon gets a visibly different label so the user can
  // tell it apart before hitting stop.
  const label = input.external
    ? mt('tray.external', { label: base, pid: input.externalPid ?? '?' })
    : input.foreignPid !== null ? mt('tray.foreign', { label: base, pid: input.foreignPid }) : base
  const tooltip = `Agent24 — ${label.replace(/^[●○✕]\s*/, '')} (${input.backendKind})`
  // DEP-A5 #2: stop/restart only ever act on a daemon this app spawned, so the
  // external case is worded "disconnect", never implying a kill.
  const daemonItems: TrayItem[] =
    input.status === 'stopped'
      ? [{ label: mt('tray.start'), action: 'start' }]
      : input.external
        ? [{ label: mt('tray.disconnect'), action: 'disconnect' }]
        : [{ label: mt('tray.restart'), action: 'restart' }, { label: mt('tray.stop'), action: 'stop' }]
  return {
    title: TITLES[input.status],
    tooltip,
    items: [
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
