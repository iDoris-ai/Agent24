import { capabilitiesMessages } from './capabilities.i18n'
import { useT } from '../i18n'
import { useComponentList } from '../components/useComponentList'
import type { ComponentView } from '../components/componentApi'
import { packageIntegrityHint } from '../components/packageIntegrity'
import { idorisMessages } from '../components/idoris/IdorisPanel.i18n'
import { settingsMessages } from '../pages/Settings.i18n'
import { communicationMessages } from '../pages/comm/Communication.i18n'
import { voicePanelMessages } from '../pages/voice/VoicePanel.i18n'

const TRACKED = new Set(['localModels', 'communication', 'voice'])

function findComponent(components: readonly ComponentView[], id: string): ComponentView | undefined {
  return components.find((item) => item.id === id)
}

/**
 * Capability-card lifecycle. Installed / running comes from GET /api/v1/components.
 * Loading and failure are explicit so a missing read cannot fall through to
 * Built-in / Open.
 */
export function useCapabilityStatus(active: boolean) {
  const list = useComponentList(active)
  const cap = useT(capabilitiesMessages)
  const idorisText = useT(idorisMessages)
  const settingsText = useT(settingsMessages)
  const commText = useT(communicationMessages)
  const voiceText = useT(voicePanelMessages)

  const localModels = (component: ComponentView | undefined): string => {
    if (!component) return cap('status.missing')
    switch (component.state) {
      case 'absent': return idorisText('state.not_installed')
      case 'installing': return idorisText('phase.installing')
      case 'installed': return component.reason === 'not_configured'
        ? idorisText('state.not_configured')
        : idorisText('state.installed')
      case 'starting': return idorisText('state.starting')
      case 'ready': return `iDoris · ${settingsText('status.connected')}`
      case 'degraded': return idorisText('state.degraded')
      case 'stopping': return idorisText('state.stopping')
      case 'stopped': return idorisText('state.stopped')
      case 'incompatible': return idorisText('state.incompatible')
      case 'failed': {
        const integrity = packageIntegrityHint(component.reason)
        if (integrity) return integrity
        return component.reason === 'instance_conflict'
          ? idorisText('state.instance_conflict')
          : idorisText('state.failed')
      }
      default: return cap('status.state', { state: component.state })
    }
  }

  const communication = (component: ComponentView | undefined): string => {
    if (!component) return cap('status.missing')
    if (component.state === 'ready') return commText('process.running')
    if (component.state === 'starting') return commText('process.starting')
    if (component.state === 'stopped') {
      if (component.reason === 'locked') return commText('process.locked')
      return component.reason === 'not_configured' ? commText('process.notConfigured') : commText('process.stopped')
    }
    if (component.state === 'failed') {
      if (component.reason === 'locked') return commText('process.locked')
      if (component.reason === 'gave_up') return commText('process.gaveUp')
      if (component.reason === 'backoff') return commText('process.backoff')
      if (component.reason === 'starting') return commText('process.starting')
      const integrity = packageIntegrityHint(component.reason)
      if (integrity) return integrity
      return commText('component.failed')
    }
    if (component.state === 'installing') return commText('component.installing')
    if (component.state === 'degraded') return commText('component.degraded')
    if (component.state === 'stopping') return commText('component.stopping')
    if (component.state === 'absent') return commText('component.absent')
    if (component.state === 'installed') return commText('component.installed')
    if (component.state === 'incompatible') return commText('component.incompatible')
    return cap('status.state', { state: component.state })
  }

  const voice = (component: ComponentView | undefined): string => {
    if (!component) return voiceText('attachment.unpaired')
    if (component.state === 'ready') return voiceText('attachment.attached')
    if (component.state === 'stopped' && component.reason === 'disabled') return voiceText('attachment.disabled')
    if (component.state === 'stopped') return `${voiceText('attachment.detached')} · ${voiceText('attachment.open')}`
    if (component.state === 'starting') return voiceText('attachment.checking')
    if (component.state === 'failed') {
      const integrity = packageIntegrityHint(component.reason)
      if (integrity) return integrity
    }
    return cap('status.state', { state: component.state })
  }

  return (id: string): string | undefined => {
    if (!TRACKED.has(id)) return undefined
    if (!active) return undefined
    if (list.status === 'idle' || list.status === 'loading') return cap('status.loading')
    if (list.status === 'error') return cap('status.unavailable')
    if (id === 'localModels') return localModels(findComponent(list.components, 'idoris'))
    if (id === 'communication') return communication(findComponent(list.components, 'hyphae'))
    return voice(findComponent(list.components, 'agentear'))
  }
}
