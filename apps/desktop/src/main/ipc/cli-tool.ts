import { dialog, ipcMain } from 'electron'
import { IpcChannels, type CliToolStatus, type CliInstallResult } from '../../shared/ipc-types'
import { isMainLocale, mt, setMainLocale } from '../main-i18n'
import { cliToolStatus, installBundledCli } from '../cli-tool'
import type { IpcGuard } from './guard'

export function registerCliTool(options: {
  guard: IpcGuard; executable: string; resources: string; packaged: boolean; changed: () => void; openSettings: () => void
}) {
  let status: CliToolStatus = { state: 'unavailable', path: null, version: null, bundledVersion: null, paths: [] }
  let checking: Promise<CliToolStatus> | null = null
  function refresh(): Promise<CliToolStatus> {
    checking ??= cliToolStatus(options.executable, options.resources, options.packaged)
      .then((value) => { status = value; options.changed(); return value })
      .finally(() => { checking = null })
    return checking
  }
  const noArgs = (args: unknown[]) => { if (args.length) throw new Error('CLI action accepts no arguments') }
  ipcMain.handle(IpcChannels.AppLocale, options.guard((_event, value: unknown) => {
    if (!isMainLocale(value)) throw new Error('Invalid locale')
    setMainLocale(value)
  }))
  ipcMain.handle(IpcChannels.CliToolStatus, options.guard((_event, ...args: unknown[]) => {
    noArgs(args)
    return refresh()
  }))
  for (const channel of [IpcChannels.CliToolInstall, IpcChannels.CliToolRepairUser]) {
    let pending: Promise<CliInstallResult> | null = null
    ipcMain.handle(channel, options.guard((_event, ...args: unknown[]) => {
      noArgs(args)
      pending ??= installBundledCli({
        ...options,
        userOnly: channel === IpcChannels.CliToolRepairUser,
        confirm: async () => (await dialog.showMessageBox({
          type: 'warning', title: mt('cli.confirmTitle'), message: mt('cli.confirmTitle'), detail: mt('cli.confirmDetail'),
          buttons: [mt('dialog.cancel'), mt('cli.replace')], defaultId: 0, cancelId: 0, noLink: true,
        })).response === 1,
      }).then(async (result) => {
        // Drain an older status probe before starting the post-install one.
        await checking
        await refresh()
        return result
      }).finally(() => { pending = null })
      return pending
    }))
  }
  return {
    refresh,
    status: () => status,
    openSettings: options.openSettings,
  }
}
