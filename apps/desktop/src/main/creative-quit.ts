import { CREATIVE_QUIT_STOP_BUDGET_MS, stopWithin } from './creative-serve-web'

type ShutdownSignal = 'SIGINT' | 'SIGTERM' | 'SIGHUP'
const exitCodes = { SIGINT: 130, SIGTERM: 143, SIGHUP: 129 } as const

export function createCreativeShutdown(deps: {
  prepare: () => void
  stop: () => Promise<void>
  cleanup: () => void
  quit: () => void
  exit: (code: number) => void
}) {
  let shutdown: Promise<void> | undefined
  let done = false
  let signalCode: number | undefined
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
    if (!started) void stopping.finally(() => { if (signalCode === undefined) deps.quit() }).catch(() => {})
  }
  return {
    beforeQuit: quit,
    willQuit: quit,
    signal: (signal: ShutdownSignal): void => {
      if (signalCode !== undefined) { deps.exit(signalCode); return }
      signalCode = exitCodes[signal]
      void run().finally(() => deps.exit(signalCode ?? exitCodes[signal])).catch(() => {})
    },
  }
}
