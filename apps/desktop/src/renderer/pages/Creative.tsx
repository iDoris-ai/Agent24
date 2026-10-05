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

/** The on-demand iDoris Design component card — shown instead of the Creative
 * WebContentsView whenever the component isn't installed yet. User-facing
 * copy says "iDoris Design" (the fork's own branding); internal identifiers,
 * directory names, and the tarball itself stay "open-design". */
function ComponentCard({
  status,
  onInstall,
}: {
  status: OpenDesignComponentStatusResult
  onInstall: () => void
}): JSX.Element {
  if (status.state === 'unavailable') {
    return (
      <div className="creative-download">
        <div>此构建未附带 iDoris Design 组件下载信息</div>
        <div className="creative-download-hint">
          开发模式：设置环境变量 A24_OPEN_DESIGN_DIR 指向本地 Open Design 检出目录即可直接使用。
        </div>
      </div>
    )
  }

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

  // 'not-installed'
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

  const installComponent = (): void => {
    void window.agent24.openDesignComponentInstall().then(setComponent)
  }

  useEffect(() => {
    let cancelled = false
    void window.agent24.openDesignComponentStatus().then((status) => {
      if (!cancelled) setComponent(status)
    })
    const unsubscribe = window.agent24.onOpenDesignComponentProgress((status) => {
      if (!cancelled) setComponent(status)
    })
    return () => {
      cancelled = true
      unsubscribe()
    }
  }, [])

  const installed = component.state === 'installed'

  useEffect(() => {
    const host = hostRef.current
    if (!host || !installed) return

    let cancelled = false
    const syncBounds = () => {
      if (!cancelled) void window.agent24.creativeBounds(rectOf(host))
    }
    const observer = new ResizeObserver(syncBounds)
    observer.observe(host)

    void window.agent24.creativeShow(rectOf(host)).then(
      (result) => {
        if (cancelled) return
        if (!result.ok && !result.needsDownload) setError(result.error ?? 'Open Design failed to start')
      },
      (reason) => {
        if (!cancelled) setError(reason instanceof Error ? reason.message : 'Open Design failed to start')
      },
    )

    window.addEventListener('resize', syncBounds)
    return () => {
      cancelled = true
      observer.disconnect()
      window.removeEventListener('resize', syncBounds)
      void window.agent24.creativeHide()
    }
  }, [installed])

  const restart = async (): Promise<void> => {
    const host = hostRef.current
    if (!host || restarting) return
    setRestarting(true)
    try {
      const result = await window.agent24.creativeRestart(rectOf(host))
      setError(result.ok ? null : (result.error ?? 'Open Design failed to restart'))
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Open Design failed to restart')
    } finally {
      setRestarting(false)
    }
  }

  return (
    <div ref={hostRef} className="creative-host">
      {!installed && <ComponentCard status={component} onInstall={installComponent} />}
      {installed && error && (
        <div className="creative-error">
          <div>Open Design unavailable: {error}</div>
          <button className="btn btn-primary" type="button" disabled={restarting} onClick={() => void restart()}>
            {restarting ? 'Restarting…' : 'Retry Open Design'}
          </button>
        </div>
      )}
    </div>
  )
}
