// BoxLite Host — availability check and CodeBox factory.
// Requires: Apple Silicon macOS 12+ (Hypervisor.framework) or Linux x86_64/ARM64 with KVM.
// On unsupported hardware the native NAPI binding will fail to load — we surface a clear
// hardware-requirements message rather than a degraded experience.

let CodeBoxClass: (new () => { run(code: string): Promise<string>; stop(): Promise<void> }) | null = null
let initError: string | null = null
let initialized = false

function ensureInit(): void {
  if (initialized) return
  initialized = true
  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const mod = require('@boxlite-ai/boxlite') as { CodeBox: typeof CodeBoxClass }
    CodeBoxClass = mod.CodeBox
    console.log('[boxlite] native binding loaded')
  } catch (err) {
    const raw = err instanceof Error ? err.message : String(err)
    // Replace low-level NAPI/binding errors with a human-readable hardware requirement message.
    initError = `需要 Apple Silicon macOS 12+（Hypervisor.framework）或 Linux x86_64/ARM64（KVM）。技术详情：${raw}`
    console.warn('[boxlite] hardware not supported:', raw)
  }
}

export function isBoxliteAvailable(): boolean {
  ensureInit()
  return CodeBoxClass !== null
}

export function getBoxliteError(): string | null {
  ensureInit()
  return initError
}

/** Run Python code in an isolated CodeBox. Box is started fresh and stopped after each run. */
export async function runPython(code: string): Promise<{ ok: true; output: string } | { ok: false; error: string }> {
  ensureInit()
  if (!CodeBoxClass) {
    return { ok: false, error: `BoxLite unavailable: ${initError}` }
  }
  const box = new CodeBoxClass()
  try {
    const output = await box.run(code)
    return { ok: true, output }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err)
    return { ok: false, error: message }
  } finally {
    await box.stop().catch(() => {/* best-effort cleanup */})
  }
}
