import { useEffect, useState } from 'react'
import type { IdorisStatusResult } from '../../shared/ipc-types'
import { useT } from '../i18n'
import { settingsMessages } from '../pages/Settings.i18n'
import { modelsMessages } from '../pages/Models.i18n'
import { CommApiError, getCommStatus, type CommDaemonProcessState } from '../pages/comm/api'
import { communicationMessages } from '../pages/comm/Communication.i18n'

const PROCESS_LABELS = {
  stopped: 'process.stopped', starting: 'process.starting', running: 'process.running',
  backoff: 'process.backoff', gave_up: 'process.gaveUp', locked: 'process.locked',
  not_configured: 'process.notConfigured',
} as const satisfies Record<CommDaemonProcessState, keyof typeof communicationMessages.en>

export function useCapabilityStatus(active: boolean) {
  const [idoris, setIdoris] = useState<IdorisStatusResult>()
  const [comm, setComm] = useState<CommDaemonProcessState>()
  const modelText = useT(modelsMessages)
  const settingsText = useT(settingsMessages)
  const commText = useT(communicationMessages)

  useEffect(() => {
    if (!active) return
    let disposed = false
    const refresh = async () => {
      await Promise.allSettled([
        window.agent24.idorisStatus?.().then(value => { if (!disposed) setIdoris(value) }),
        getCommStatus().then(value => { if (!disposed) setComm(value.process.state) }).catch((error: unknown) => {
          if (!disposed && error instanceof CommApiError && error.code === 'not_configured') setComm('not_configured')
        }),
      ])
    }
    // Poll serially so a slow response cannot be replaced by an older request.
    let timer: ReturnType<typeof setTimeout>
    const poll = async () => {
      await refresh()
      if (!disposed) timer = setTimeout(() => void poll(), 5000)
    }
    void poll()
    return () => { disposed = true; clearTimeout(timer) }
  }, [active])

  return (id: string): string | undefined => {
    if (id === 'localModels' && idoris) {
      return idoris.state === 'available' ? `iDoris · ${settingsText('status.connected')}`
        : modelText(idoris.reason === 'handoff-missing' ? 'idoris.notConfigured' : 'idoris.unavailable')
    }
    if (id === 'communication' && comm) return commText(PROCESS_LABELS[comm])
    return undefined
  }
}
