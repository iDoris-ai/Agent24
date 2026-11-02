import { useEffect, useState } from 'react'
import { listComponents, type ComponentView } from './componentApi'

export const COMPONENT_LIST_POLL_MS = 5000

export type ComponentListModel =
  | { status: 'idle' }
  | { status: 'loading' }
  | { status: 'error' }
  | { status: 'ready'; components: ComponentView[] }

/**
 * Polls GET /api/v1/components while `active`. A later failure replaces a
 * previous success so the page cannot keep showing a stale lifecycle.
 */
export function useComponentList(active: boolean): ComponentListModel {
  const [model, setModel] = useState<ComponentListModel>({ status: 'idle' })

  useEffect(() => {
    if (!active) {
      setModel({ status: 'idle' })
      return
    }
    let disposed = false
    setModel({ status: 'loading' })
    let timer: ReturnType<typeof setTimeout>
    const poll = async () => {
      try {
        const components = await listComponents()
        if (!disposed) setModel({ status: 'ready', components })
      } catch {
        if (!disposed) setModel({ status: 'error' })
      }
      if (!disposed) timer = setTimeout(() => void poll(), COMPONENT_LIST_POLL_MS)
    }
    void poll()
    return () => { disposed = true; clearTimeout(timer) }
  }, [active])

  return model
}
