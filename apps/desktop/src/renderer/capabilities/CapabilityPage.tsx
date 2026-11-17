import { useEffect, useState } from 'react'
import { useT } from '../i18n'
import type { OsPackageProgress } from '../../shared/ipc-types'
import { compareVersions, isRegisteredOsPackage } from '../../shared/os-package-registry'
import { packageIntegrityHint } from '../components/packageIntegrity'
import { useComponentList } from '../components/useComponentList'
import { listOsModules, type DomainOsView } from '../pages/agent/api'
import { capabilitiesMessages } from './capabilities.i18n'
import type { CapabilityDef } from './types'

function versionLabel(version: string): string {
  return version.replace(/^v/, '')
}

export function CapabilityPage({ capability }: { capability: CapabilityDef }) {
  if (capability.group === 'domainOs') return <DomainOsPage capability={capability} />
  return <Placeholder capability={capability} />
}

function Placeholder({ capability }: { capability: CapabilityDef }) {
  const c = useT(capabilitiesMessages)
  const next = capability.id === 'mediaBase' ? 'next.mediaBase'
    : capability.id === 'create' ? 'next.create' : 'next.service'
  return <main className="content">
    <h1 className="page-title">{c(capability.nameKey)}</h1>
    <p className="page-sub">{c(capability.descKey)}</p>
    <p role="status">{c('status.notSetUp')}</p>
    <p>{c(next)}</p>
  </main>
}

function DomainOsPage({ capability }: { capability: CapabilityDef }) {
  const c = useT(capabilitiesMessages)
  const registered = isRegisteredOsPackage(capability.id)
  const components = useComponentList(true)
  const [modules, setModules] = useState<DomainOsView[] | null>(null)
  const [osError, setOsError] = useState(false)
  const [latest, setLatest] = useState<string | null>(null)
  const [reload, setReload] = useState(0)
  const [busy, setBusy] = useState(false)
  const [progress, setProgress] = useState<OsPackageProgress | null>(null)
  const [failure, setFailure] = useState<string | null>(null)

  useEffect(() => {
    let cancel = false
    setOsError(false)
    void listOsModules().then((list) => {
      if (!cancel) setModules(list.modules)
    }).catch(() => {
      if (!cancel) setOsError(true)
    })
    return () => { cancel = true }
  }, [capability.id, reload])

  useEffect(() => {
    if (!registered) return
    let cancel = false
    void window.agent24.osPackageLatest(capability.id).then((res) => {
      if (cancel || !res.ok) return
      const version = (res.data as { version?: unknown }).version
      if (typeof version === 'string') setLatest(version)
    }).catch(() => { /* the install button still works without a latest label */ })
    return () => { cancel = true }
  }, [capability.id, registered, reload])

  useEffect(() => window.agent24.onOsPackageProgress((next) => {
    if (next.id === capability.id) setProgress(next)
  }), [capability.id])

  const mod = modules?.find((item) => item.name === capability.id)
  const component = components.status === 'ready' ? components.components.find((item) => item.id === capability.id) : undefined
  const failed = component?.state === 'failed'
  const integrity = failed ? packageIntegrityHint(component?.reason) ?? (component?.reason ? c('os.failed', { reason: component.reason }) : undefined) : undefined
  const mounted = mod?.state === 'mounted'
  const installedVersion = mounted ? versionLabel(mod.version) : null
  const newer = installedVersion !== null && latest !== null && compareVersions(latest, installedVersion) > 0
  const phaseLabel = !progress ? null
    : progress.phase === 'download' ? c('os.phase.download')
      : progress.phase === 'verify' ? c('os.phase.verify')
        : progress.phase === 'extract' ? c('os.phase.extract')
          : c('os.phase.install')

  async function install(): Promise<void> {
    setBusy(true)
    setFailure(null)
    setProgress(null)
    try {
      const res = await window.agent24.osPackageInstall(capability.id)
      if (!res.ok) {
        const message = (res.data as { error?: { message?: unknown } }).error?.message
        setFailure(typeof message === 'string' ? message : c('status.unavailable'))
      }
      setReload((n) => n + 1)
    } catch (error) {
      setFailure(error instanceof Error ? error.message : c('status.unavailable'))
    } finally {
      setBusy(false)
      setProgress(null)
    }
  }

  let status = c('status.loading')
  if (integrity) status = integrity
  else if (mounted && installedVersion) status = c('os.installed', { version: installedVersion })
  else if (modules && !mounted) status = c('os.notInstalled')
  else if (osError) status = c('status.unavailable')

  return <main className="content">
    <h1 className="page-title">{c(capability.nameKey)}</h1>
    <p className="page-sub">{c(capability.descKey)}</p>
    <p role="status">{status}</p>
    {registered && !mounted && !integrity && modules && (
      <button type="button" disabled={busy} onClick={() => void install()}>{c('os.installLatest')}</button>
    )}
    {registered && integrity && (
      <button type="button" disabled={busy} onClick={() => void install()}>{c('os.installLatest')}</button>
    )}
    {registered && newer && latest && (
      <button type="button" disabled={busy} onClick={() => void install()}>{c('os.updateTo', { version: versionLabel(latest) })}</button>
    )}
    {!registered && <p>{c('os.notOffered')}</p>}
    {phaseLabel && <p role="status">{phaseLabel}</p>}
    {failure && <p role="alert">{failure}</p>}
  </main>
}
