import { dialog, ipcMain } from 'electron'
import { IpcChannels, type CliToolStatus, type CliInstallResult } from '../../shared/ipc-types'
import { DEFAULT_LOCALE, isLocale, translate, type Locale } from '../../shared/i18n'
import { cliToolMessages } from '../../shared/cli-tool.i18n'
import { cliToolStatus, installBundledCli } from '../cli-tool'
import type { IpcGuard } from './guard'

export function registerCliTool(options: {
  guard: IpcGuard; executable: string; resources: string; packaged: boolean; changed: () => void; openSettings: () => void
}) {
  let locale: Locale = DEFAULT_LOCALE
  let status: CliToolStatus = { state: 'unavailable', path: null, version: null, bundledVersion: null }
  let pending: Promise<CliInstallResult> | null = null
  let checking: Promise<CliToolStatus> | null = null
  const t = (key: keyof typeof cliToolMessages.en) => translate(cliToolMessages, key, { version: status.version ?? '?' }, locale)
  function refresh(): Promise<CliToolStatus> {
    checking ??= cliToolStatus(options.executable, options.resources, options.packaged)
      .then((value) => { status = value; options.changed(); return value })
      .finally(() => { checking = null })
    return checking
  }
  const noArgs = (args: unknown[]) => { if (args.length) throw new Error('CLI action accepts no arguments') }
  ipcMain.handle(IpcChannels.AppLocale, options.guard((_event, value: unknown) => {
    if (!isLocale(value)) throw new Error('Invalid locale')
    locale = value
    options.changed()
  }))
  ipcMain.handle(IpcChannels.CliToolStatus, options.guard((_event, ...args: unknown[]) => {
    noArgs(args)
    return refresh()
  }))
  ipcMain.handle(IpcChannels.CliToolInstall, options.guard((_event, ...args: unknown[]) => {
    noArgs(args)
    pending ??= installBundledCli({
      ...options,
      confirm: async () => (await dialog.showMessageBox({
        type: 'warning', title: t('confirmTitle'), message: t('confirmTitle'), detail: t('confirmDetail'),
        buttons: [t('cancel'), t('replace')], defaultId: 0, cancelId: 0, noLink: true,
      })).response === 1,
    }).then(async (result) => {
      // Drain an older status probe before starting the post-install one.
      await checking
      await refresh()
      return result
    }).finally(() => { pending = null })
    return pending
  }))
  return {
    refresh,
    trayItem: () => ['missing', 'outdated', 'unknown'].includes(status.state)
      ? { label: t(status.state), click: options.openSettings } : null,
  }
}
