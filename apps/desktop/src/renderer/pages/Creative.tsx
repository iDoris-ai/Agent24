import { useEffect, useRef, useState } from 'react'

function rectOf(el: HTMLElement): { x: number; y: number; width: number; height: number } {
  const rect = el.getBoundingClientRect()
  return { x: rect.x, y: rect.y, width: rect.width, height: rect.height }
}

export default function CreativePage(): JSX.Element {
  const hostRef = useRef<HTMLDivElement>(null)
  const [error, setError] = useState<string | null>(null)
  const [restarting, setRestarting] = useState(false)

  useEffect(() => {
    const host = hostRef.current
    if (!host) return

    let cancelled = false
    const syncBounds = () => {
      if (!cancelled) void window.agent24.creativeBounds(rectOf(host))
    }
    const observer = new ResizeObserver(syncBounds)
    observer.observe(host)

    void window.agent24.creativeShow(rectOf(host)).then(
      (result) => {
        if (!cancelled && !result.ok) setError(result.error ?? 'Open Design failed to start')
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
  }, [])

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
      {error && (
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
