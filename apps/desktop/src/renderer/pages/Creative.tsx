import { useEffect, useRef, useState } from 'react'
import type { OpenDesignComponentStatusResult } from '../../shared/ipc-types'

function rectOf(el: HTMLElement): { x: number; y: number; width: number; height: number } {
  const rect = el.getBoundingClientRect()
  return { x: rect.x, y: rect.y, width: rect.width, height: rect.height }
}

function formatMb(bytes: number | undefined): string {
  if (!bytes || bytes <= 0) return '?'
  return Math.max(1, Math.round(bytes / (1024 * 1024))).toString()
}

const DOWNLOAD_FLOW_STATES = new Set(['not-installed', 'downloading', 'verifying', 'failed'])

/** The on-demand iDoris Design component card — shown instead of the Creative
 * WebContentsView whenever the component isn't installed yet. User-facing
 * copy says "iDoris Design" (the fork's own branding); internal identifiers,
 * directory names, and the tarball itself stay "open-design".
 *
 * PR #670 review (H1): `status` here is NEVER `installed` or `unavailable` —
 * see CreativePage below for why: both of those states attempt
 * `creativeShow` directly (possibly via a dev checkout CreativeServeWeb
 * knows about but this card doesn't), and only its `needsDownload` result
 * (synthesized back into a `not-installed`-shaped status) routes here. */
function ComponentCard({
  status,
  onInstall,
}: {
  status: OpenDesignComponentStatusResult
  onInstall: () => void
}): JSX.Element {
  if (status.state === 'downloading') {
    const total = status.total ?? 0
    const received = status.received ?? 0
    const pct = total > 0 ? Math.min(100, Math.round((received / total) * 100)) : 0
    return (
      <div className="creative-download">
        <div>正在下载 iDoris Design 组件…</div>
        <div className="progress-bar"><div className="progress-fill" style={{ width: `${pct}%` }} /></div>
        <div className="creative-download-hint">{formatMb(received)} / {formatMb(total)} MB（{pct}%）</div>
      </div>
    )
  }

  if (status.state === 'verifying') {
    return (
      <div className="creative-download">
        <div>正在校验…</div>
      </div>
    )
  }

  if (status.state === 'failed') {
    return (
      <div className="creative-download">
        <div>iDoris Design 组件下载失败：{status.error ?? '未知错误'}</div>
        <button className="btn btn-primary" type="button" onClick={onInstall}>重试</button>
      </div>
    )
  }

  // 'not-installed' (also the synthesized shape for a needs-download result)
  return (
    <div className="creative-download">
      <div>Design 需要额外下载 iDoris Design 组件（约 {formatMb(status.size)} MB），下载完成后即可使用。</div>
      <button className="btn btn-primary" type="button" onClick={onInstall}>下载</button>
    </div>
  )
}

export default function CreativePage(): JSX.Element {
  const hostRef = useRef<HTMLDivElement>(null)
  const [error, setError] = useState<string | null>(null)
  const [restarting, setRestarting] = useState(false)
  const [component, setComponent] = useState<OpenDesignComponentStatusResult>({ state: 'not-installed' })
  // PR #670 review (H1): set once `creativeShow` itself reports
  // `needsDownload` — this is the ONLY thing that puts up the download card
  // for a component snapshot of `installed` or `unavailable`, because only
  // CreativeServeWeb (not this page, not the component installer) knows
  // whether a dev checkout makes the download unnecessary.
  const [needsDownloadSize, setNeedsDownloadSize] = useState<number | undefined>(undefined)

  const applyComponentStatus = (status: OpenDesignComponentStatusResult): void => {
    setComponent(status)
    if (status.state === 'installed') setNeedsDownloadSize(undefined)
  }

  const installComponent = (): void => {
    void window.agent24.openDesignComponentInstall().then(applyComponentStatus)
  }

  useEffect(() => {
    let cancelled = false
    void window.agent24.openDesignComponentStatus().then((status) => {
      if (!cancelled) applyComponentStatus(status)
    })
    const unsubscribe = window.agent24.onOpenDesignComponentProgress((status) => {
      if (!cancelled) applyComponentStatus(status)
    })
    return () => {
      cancelled = true
      unsubscribe()
    }
  }, [])

  const showDownloadCard = needsDownloadSize !== undefined || DOWNLOAD_FLOW_STATES.has(component.state)
  // Try Creative whenever we're NOT showing the download/verify/failed card
  // — i.e. for `installed` and `unavailable`. CreativeServeWeb (not this
  // page) decides whether `unavailable` still works via a dev checkout.
  const tryCreative = !showDownloadCard

  useEffect(() => {
    const host = hostRef.current
    if (!host || !tryCreative) return

    let cancelled = false
    const syncBounds = () => {
      if (!cancelled) void window.agent24.creativeBounds(rectOf(host))
    }
    const observer = new ResizeObserver(syncBounds)
    observer.observe(host)

    void window.agent24.creativeShow(rectOf(host)).then(
      (result) => {
        if (cancelled) return
        if (result.ok) { setError(null); return }
        if (result.needsDownload) { setNeedsDownloadSize(result.size ?? 0); return }
        setError(result.error ?? 'iDoris Design 启动失败')
      },
      (reason) => {
        if (!cancelled) setError(reason instanceof Error ? reason.message : 'iDoris Design 启动失败')
      },
    )

    window.addEventListener('resize', syncBounds)
    return () => {
      cancelled = true
      observer.disconnect()
      window.removeEventListener('resize', syncBounds)
      void window.agent24.creativeHide()
    }
  }, [tryCreative])

  const restart = async (): Promise<void> => {
    const host = hostRef.current
    if (!host || restarting) return
    setRestarting(true)
    try {
      const result = await window.agent24.creativeRestart(rectOf(host))
      setError(result.ok ? null : (result.error ?? 'iDoris Design 启动失败'))
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'iDoris Design 启动失败')
    } finally {
      setRestarting(false)
    }
  }

  // When `showDownloadCard` is true only because of `needsDownloadSize` (the
  // component snapshot itself was `installed`/`unavailable`, both stale by
  // definition here), synthesize a `not-installed`-shaped status for the card.
  const cardStatus: OpenDesignComponentStatusResult = DOWNLOAD_FLOW_STATES.has(component.state)
    ? component
    : { state: 'not-installed', size: needsDownloadSize }

  return (
    <div ref={hostRef} className="creative-host">
      {showDownloadCard && <ComponentCard status={cardStatus} onInstall={installComponent} />}
      {tryCreative && error && (
        <div className="creative-error">
          <div>iDoris Design 暂不可用：{error}</div>
          <button className="btn btn-primary" type="button" disabled={restarting} onClick={() => void restart()}>
            {restarting ? '正在启动 iDoris Design…' : '重试'}
          </button>
        </div>
      )}
    </div>
  )
}
