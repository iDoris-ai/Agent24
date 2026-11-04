// Electron converts SIGTERM / SIGINT / SIGHUP into app.quit(); hook only before-quit and will-quit.
import { CREATIVE_QUIT_STOP_BUDGET_MS, stopWithin } from './creative-serve-web'

export function createCreativeShutdown(deps: {
  prepare: () => void
  stop: () => Promise<void>
  cleanup: () => void
  quit: () => void
}) {
  let shutdown: Promise<void> | undefined
  let done = false
  const run = (): Promise<void> => {
    shutdown ??= Promise.resolve().then(async () => {
      try {
        // A failing teardown step must not skip stopping Open Design.
        try { deps.prepare() } catch (error) { console.error('[main] quit preparation failed', error) }
        if (!await stopWithin({ stop: deps.stop }, CREATIVE_QUIT_STOP_BUDGET_MS)) {
          console.warn(`[main] Open Design did not stop cleanly within ${CREATIVE_QUIT_STOP_BUDGET_MS} ms`)
        }
      } finally {
        try { deps.cleanup() } finally { done = true }
      }
    })
    return shutdown
  }
  const quit = (event: { preventDefault: () => void }): void => {
    if (done) return
    event.preventDefault()
    const started = shutdown !== undefined
    const stopping = run()
    if (!started) void stopping.finally(() => deps.quit()).catch(() => {})
  }
  return {
    beforeQuit: quit,
    willQuit: quit,
  }
}
