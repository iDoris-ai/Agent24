import { ipcMain } from 'electron'
import { IpcChannels } from '../../shared/ipc-types'
import { disable, enable, state, sync, type DocAgentAccessDeps } from '../doc-agent-access'
import type { IpcGuard } from './guard'

const ACTIONS = ['state', 'enable', 'disable', 'sync'] as const
type Action = (typeof ACTIONS)[number]

function isAction(value: unknown): value is Action {
  return typeof value === 'string' && (ACTIONS as readonly string[]).includes(value)
}

/** D2b: renderer asks to read, grant, revoke or refresh Agent access to documents. */
export function registerDocAgentAccess(options: {
  guard: IpcGuard
  deps: DocAgentAccessDeps
  confirm: () => Promise<boolean>
}): void {
  ipcMain.handle(IpcChannels.DocumentsAgentAccess, options.guard(async (_event, ...args: unknown[]) => {
    if (args.length !== 1 || !isAction(args[0])) {
      throw new Error("documents:agent-access action must be 'state' | 'enable' | 'disable' | 'sync'")
    }
    const action = args[0]
    if (action === 'state') return state(options.deps)
    if (action === 'enable') {
      if (!(await options.confirm())) return state(options.deps)
      return enable(options.deps)
    }
    if (action === 'disable') return disable(options.deps)
    return sync(options.deps)
  }))
}
