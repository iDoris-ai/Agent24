// Shared helpers for contract tests.
// A24_BASE_URL selects the daemon under test (node mock today, agent24d later).
// A24_TOKEN carries the bearer token for agent24d (empty = no auth header).
// NOTE: plain BASE_URL is unusable — it is a Vite builtin ('/' by default) that
// vitest injects into process.env, silently shadowing any external value.

// `||` (not ??) so an empty-string env var still falls back to the default.
// Trailing slash is stripped: the daemon routes by exact pathname match, so
// `http://host:8765/` + `/health` would otherwise become `//health` → 404.
export const BASE_URL = (process.env['A24_BASE_URL'] || 'http://127.0.0.1:8765').replace(/\/$/, '')
export const TOKEN = process.env['A24_TOKEN'] || ''

export interface JsonResponse {
  status: number
  body: unknown
}

export async function request(
  method: 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE',
  path: string,
  body?: unknown,
): Promise<JsonResponse> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' }
  if (TOKEN) headers['Authorization'] = `Bearer ${TOKEN}`
  const res = await fetch(`${BASE_URL}${path}`, {
    method,
    headers,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  })
  let parsed: unknown = null
  try {
    parsed = await res.json()
  } catch {
    parsed = null
  }
  return { status: res.status, body: parsed }
}

export const get = (path: string) => request('GET', path)
export const post = (path: string, body?: unknown) => request('POST', path, body)

// LLM expectation mode — prevents silent false-greens from auto-skipping:
//   A24_EXPECT_LLM=up    assert the 200-success chat contract (fail if provider down)
//   A24_EXPECT_LLM=down  assert the 503-unavailable contract (fail if provider up)
//   unset/auto           probe once and pick the matching branch (local dev convenience)
// CI MUST set an explicit mode so both contracts are knowingly exercised.
export async function resolveLlmExpectation(): Promise<boolean> {
  const mode = process.env['A24_EXPECT_LLM'] || 'auto'
  if (mode === 'up') return true
  if (mode === 'down') return false
  if (process.env['CI']) {
    throw new Error(
      'A24_EXPECT_LLM must be explicitly "up" or "down" in CI — auto-probing would silently skip one chat contract',
    )
  }
  return llmProviderAvailable()
}

// Probe whether a local LLM provider (oMLX/Ollama) is actually reachable.
// NOTE: probes the test runner's localhost providers — valid for the node
// daemon (same-host gateway); the future agent24d dual-run suite must not
// rely on this (see current.test.ts header).
export async function llmProviderAvailable(): Promise<boolean> {
  const probes = [
    { url: 'http://127.0.0.1:8088/v1/models', headers: { Authorization: 'Bearer xiaobao8088' } },
    { url: 'http://127.0.0.1:11434/api/tags', headers: {} },
  ]
  for (const p of probes) {
    try {
      const res = await fetch(p.url, { headers: p.headers, signal: AbortSignal.timeout(2000) })
      if (res.ok) return true
    } catch {
      /* try next */
    }
  }
  return false
}
