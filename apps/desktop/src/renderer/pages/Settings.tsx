import { useContext, useRef, useState, type KeyboardEvent } from 'react'
import { CapabilitiesContext } from '../capabilities/types'
import { LOCALES, isLocale, setLocale, useLocale, useT } from '../i18n'
import { settingsMessages } from './Settings.i18n'
import { ActivityTab } from './settings/ActivityTab'
import { CliToolSettings } from './settings/CliToolSettings'
import { ModelServiceSettings } from './settings/ModelServiceSettings'
import { SidebarPrefs } from './settings/SidebarPrefs'

// Tab ownership of the blocks that used to be one scrolling page:
// General — language. Models — AI model service. Command line tool — the
// CLI installer. Voice, Connections and Advanced had no settings blocks;
// Voice links to that capability, and the other two say so.

export const SETTINGS_TABS = ['general', 'models', 'voice', 'activity', 'cli', 'connections', 'advanced'] as const
export type SettingsTab = (typeof SETTINGS_TABS)[number]
export const SETTINGS_TAB_STORAGE_KEY = 'agent24:settings-tab'

const TAB_LABEL: Record<SettingsTab, 'tabs.general' | 'tabs.models' | 'tabs.voice' | 'tabs.activity' | 'tabs.cli' | 'tabs.connections' | 'tabs.advanced'> = {
  general: 'tabs.general',
  models: 'tabs.models',
  voice: 'tabs.voice',
  activity: 'tabs.activity',
  cli: 'tabs.cli',
  connections: 'tabs.connections',
  advanced: 'tabs.advanced',
}

function isSettingsTab(v: string | null): v is SettingsTab {
  return v !== null && (SETTINGS_TABS as readonly string[]).includes(v)
}

function readStoredTab(): SettingsTab {
  try {
    const v = localStorage.getItem(SETTINGS_TAB_STORAGE_KEY)
    if (isSettingsTab(v)) return v
  } catch { /* storage unavailable — open General */ }
  return 'general'
}

function rememberTab(tab: SettingsTab) {
  try { localStorage.setItem(SETTINGS_TAB_STORAGE_KEY, tab) } catch { /* the choice still applies this visit */ }
}

export default function SettingsPage() {
  const t = useT(settingsMessages)
  const [tab, setTab] = useState(readStoredTab)
  const tabRefs = useRef<Partial<Record<SettingsTab, HTMLButtonElement | null>>>({})
  // The model form probes and may auto-start a service. Keep it mounted after
  // the first visit so leaving the tab does not wipe a key the user typed.
  const keepModels = useRef(tab === 'models')
  if (tab === 'models') keepModels.current = true

  function activate(next: SettingsTab, focus: boolean) {
    setTab(next)
    rememberTab(next)
    if (focus) tabRefs.current[next]?.focus()
  }

  function onTabKeyDown(e: KeyboardEvent<HTMLDivElement>) {
    const i = SETTINGS_TABS.indexOf(tab)
    let nextIndex = -1
    if (e.key === 'ArrowRight') nextIndex = (i + 1) % SETTINGS_TABS.length
    else if (e.key === 'ArrowLeft') nextIndex = (i - 1 + SETTINGS_TABS.length) % SETTINGS_TABS.length
    else return
    e.preventDefault()
    activate(SETTINGS_TABS[nextIndex], true)
  }

  return (
    <div className="content">
      <div className="page-title">{t('page.title')}</div>
      <div className="page-sub">{t('page.subtitle')}</div>

      <div className="settings-tabs" role="tablist" aria-label={t('tabs.label')} onKeyDown={onTabKeyDown}>
        {SETTINGS_TABS.map((id) => (
          <button
            key={id}
            ref={(el) => { tabRefs.current[id] = el }}
            type="button"
            role="tab"
            id={`settings-tab-${id}`}
            aria-selected={tab === id}
            aria-controls="settings-panel"
            tabIndex={tab === id ? 0 : -1}
            onClick={() => activate(id, false)}
          >
            {t(TAB_LABEL[id])}
          </button>
        ))}
      </div>

      <div role="tabpanel" id="settings-panel" aria-labelledby={`settings-tab-${tab}`}>
        {tab === 'general' && <GeneralTab />}
        {keepModels.current && (
          <div hidden={tab !== 'models'}>
            <ModelServiceSettings />
          </div>
        )}
        {tab === 'voice' && <VoiceTab />}
        {tab === 'activity' && <ActivityTab />}
        {tab === 'cli' && <CliToolSettings />}
        {tab === 'connections' && <ConnectionsTab />}
        {tab === 'advanced' && <AdvancedTab />}
      </div>
    </div>
  )
}

function GeneralTab() {
  const t = useT(settingsMessages)
  const locale = useLocale()
  return (
    <>
      <div className="settings-section">
        <h3>{t('appearance.title')}</h3>
        <div className="setting-row">
          <div><label htmlFor="settings-language">{t('appearance.language')}</label><p>{t('appearance.languageHint')}</p></div>
          <select id="settings-language" value={locale}
            onChange={(e) => { if (isLocale(e.target.value)) setLocale(e.target.value) }}>
            {LOCALES.map((l) => <option key={l.id} value={l.id}>{l.label}</option>)}
          </select>
        </div>
      </div>
      <SidebarPrefs />
    </>
  )
}

function VoiceTab() {
  const t = useT(settingsMessages)
  const { defs, open } = useContext(CapabilitiesContext)
  const voice = defs.find((d) => d.id === 'voice') ?? defs.find((d) => d.page === 'voice')
  return (
    <div className="settings-section">
      <h3>{t('voice.title')}</h3>
      <p className="settings-note">{t('voice.none')}</p>
      <button type="button" className="btn btn-ghost" style={{ marginTop: 12 }}
        onClick={() => open(voice?.id ?? 'voice', 'other')}>
        {t('voice.open')}
      </button>
    </div>
  )
}

function ConnectionsTab() {
  const t = useT(settingsMessages)
  return (
    <div className="settings-section">
      <h3>{t('connections.title')}</h3>
      <p className="settings-note">{t('connections.empty')}</p>
    </div>
  )
}

function AdvancedTab() {
  const t = useT(settingsMessages)
  return (
    <div className="settings-section">
      <h3>{t('advanced.title')}</h3>
      {/* Daemon port, Save and Reset were unwired and removed (AUDIT-2).
          Do not put those dead controls back. */}
      <p className="settings-note">{t('advanced.empty')}</p>
    </div>
  )
}
