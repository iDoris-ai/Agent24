import { useEffect, useRef, useState } from 'react'

function rectOf(el: HTMLElement): { x: number; y: number; width: number; height: number } {
  const rect = el.getBoundingClientRect()
  return { x: rect.x, y: rect.y, width: rect.width, height: rect.height }
}

export default function CreativePage(): JSX.Element {
  const hostRef = useRef<HTMLDivElement>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const host = hostRef.current
    if (!host) return

    let cancelled = false
    const syncBounds = () => {
      if (!cancelled) void window.agent24.creativeBounds(rectOf(host))
    }
    const observer = new ResizeObserver(syncBounds)
    observer.observe(host)

    void window.agent24.creativeShow(rectOf(host)).then((result) => {
      if (!cancelled && !result.ok) setError(result.error ?? 'Open Design failed to start')
    })

    window.addEventListener('resize', syncBounds)
    return () => {
      cancelled = true
      observer.disconnect()
      window.removeEventListener('resize', syncBounds)
      void window.agent24.creativeHide()
    }
  }, [])

  return (
    <div ref={hostRef} className="creative-host">
      {error && <div className="creative-error">Open Design unavailable: {error}</div>}
    </div>
  )
}
