import { useEffect, useState } from 'react'
import { getStatus, type IdorisStatus } from '../components/idoris/api'
import { idorisMessages } from '../components/idoris/IdorisPanel.i18n'
import { useT } from '../i18n'
import { settingsMessages } from '../pages/Settings.i18n'
import { CommApiError, getCommStatus, type CommDaemonProcessState } from '../pages/comm/api'
import { communicationMessages } from '../pages/comm/Communication.i18n'
import { getAgentEarAttachment, type AttachedView } from '../pages/voice/api'
import { voicePanelMessages } from '../pages/voice/VoicePanel.i18n'

const PROCESS_LABELS = {
  stopped: 'process.stopped', starting: 'process.starting', running: 'process.running',
  backoff: 'process.backoff', gave_up: 'process.gaveUp', locked: 'process.locked',
  not_configured: 'process.notConfigured',
} as const satisfies Record<CommDaemonProcessState, keyof typeof communicationMessages.en>

export function useCapabilityStatus(active: boolean) {
  const [idoris, setIdoris] = useState<IdorisStatus['state']>()
  const [comm, setComm] = useState<CommDaemonProcessState>()
  const [voice, setVoice] = useState<AttachedView | null>()
  const idorisText = useT(idorisMessages)
  const settingsText = useT(settingsMessages)
  const commText = useT(communicationMessages)
  const voiceText = useT(voicePanelMessages)

  useEffect(() => {
    if (!active) return
    let disposed = false
    const refresh = async () => {
      await Promise.allSettled([
        getStatus().then(value => { if (!disposed) setIdoris(value.state) }).catch(() => {
          if (!disposed) setIdoris(undefined)
        }),
        getCommStatus().then(value => { if (!disposed) setComm(value.process.state) }).catch((error: unknown) => {
          if (!disposed && error instanceof CommApiError && error.code === 'not_configured') setComm('not_configured')
        }),
        getAgentEarAttachment().then(value => { if (!disposed) setVoice(value) }).catch(() => {
          if (!disposed) setVoice(undefined)
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
      return idoris === 'running' ? `iDoris · ${settingsText('status.connected')}`
        : idorisText(`state.${idoris}`)
    }
    if (id === 'communication' && comm) return commText(PROCESS_LABELS[comm])
    if (id === 'voice' && voice !== undefined) {
      if (voice === null) return voiceText('attachment.unpaired')
      if (voice.attach_status === 'attached') return voiceText('attachment.attached')
      if (voice.attach_status === 'disabled') return voiceText('attachment.disabled')
      if (voice.attach_status === 'detached') return `${voiceText('attachment.detached')} · ${voiceText('attachment.open')}`
    }
    return undefined
  }
}
