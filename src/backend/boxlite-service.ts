// BoxLite Service Container Manager — M4
// Manages long-running OCI service containers via BoxLite SimpleBox.
// Each module with manifest.container gets its own isolated VM, accessible
// via port-forwarded localhost:<hostPort>. The backend proxies /api/svc/<id>/* to it.

import http from 'node:http'

export interface ContainerConfig {
  image: string
  port: number           // guest port inside container
  startCmd?: string[]    // argv passed directly to exec (no shell) — avoids injection
  healthPath?: string    // polled until 2xx (default: '/health')
  memoryMib?: number
}

interface ServiceEntry {
  hostPort: number
  config: ContainerConfig
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  box: any               // SimpleBox — typed as any to survive missing native binding
}

type StartResult = { ok: boolean; hostPort?: number; error?: string }

// Host port range: 18000–18999 (1000 slots).
// Ports are never recycled — the pool is sized to last the process lifetime.
const PORT_MIN = 18000
const PORT_MAX = 18999
let nextHostPort = PORT_MIN

function allocatePort(): number {
  if (nextHostPort > PORT_MAX) throw new Error(`Service port pool exhausted (${PORT_MIN}-${PORT_MAX})`)
  return nextHostPort++
}

let SimpleBoxClass: (new (opts: object) => unknown) | null = null
let serviceInitError: string | null = null
let serviceInitialized = false

function ensureServiceInit(): void {
  if (serviceInitialized) return
  serviceInitialized = true
  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const mod = require('@boxlite-ai/boxlite') as { SimpleBox: typeof SimpleBoxClass }
    SimpleBoxClass = mod.SimpleBox
    console.log('[svc] SimpleBox native binding loaded')
  } catch (err) {
    serviceInitError = err instanceof Error ? err.message : String(err)
    console.warn('[svc] native binding unavailable:', serviceInitError)
  }
}

export function isServiceAvailable(): boolean {
  ensureServiceInit()
  return SimpleBoxClass !== null
}

// Registry of running service boxes: moduleId → entry
const registry = new Map<string, ServiceEntry>()
// In-flight start promises — prevents concurrent double-start for the same moduleId (H1/H5)
const starting = new Map<string, Promise<StartResult>>()
// Boxes created but not yet healthy — tracked so stopAll() can clean them up (M2)
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const pending = new Map<string, any>()

function httpGet(url: string): Promise<number> {
  return new Promise((resolve) => {
    const req = http.get(url, (res) => {
      res.resume()                   // drain body to free socket (L1)
      res.once('end', () => resolve(res.statusCode ?? 0))
    })
    req.on('error', () => resolve(0))
    req.setTimeout(2000, () => { req.destroy(); resolve(0) })
  })
}

async function waitHealthy(hostPort: number, healthPath: string, timeoutMs = 60_000): Promise<boolean> {
  const url = `http://127.0.0.1:${hostPort}${healthPath}`
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    const code = await httpGet(url)
    if (code >= 200 && code < 300) return true
    await new Promise((r) => setTimeout(r, 1500))
  }
  return false
}

async function doStartService(moduleId: string, cfg: ContainerConfig): Promise<StartResult> {
  ensureServiceInit()
  if (!SimpleBoxClass) return { ok: false, error: `BoxLite unavailable: ${serviceInitError}` }
  if (registry.has(moduleId)) return { ok: true, hostPort: registry.get(moduleId)!.hostPort }

  let hostPort: number
  try {
    hostPort = allocatePort()
  } catch (err) {
    return { ok: false, error: err instanceof Error ? err.message : String(err) }
  }

  const healthPath = cfg.healthPath ?? '/health'
  const boxOpts = {
    image: cfg.image,
    memoryMib: cfg.memoryMib ?? 512,
    ports: [{ hostPort, guestPort: cfg.port }],
    autoRemove: true,
    name: `agent24-svc-${moduleId}`,
    reuseExisting: false,
  }

  let box: unknown
  try {
    box = new SimpleBoxClass(boxOpts)
  } catch (err) {
    return { ok: false, error: err instanceof Error ? err.message : String(err) }
  }

  // M2: track box before health check so stopAll() can clean it up during startup window
  pending.set(moduleId, box)

  // H4 fix: each startCmd arg is POSIX single-quote-escaped before being passed to
  // sh -c, preventing injection even if args contain spaces or special characters.
  // The outer nohup/& allows the service process to outlive the exec call.
  if (cfg.startCmd && cfg.startCmd.length > 0) {
    const [cmd, ...args] = cfg.startCmd
    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      await (box as any).exec('sh', ['-c', `nohup ${[cmd, ...args].map((a) => {
        // Shell-safe quoting: replace single-quotes, wrap in single-quotes
        return "'" + String(a).replace(/'/g, "'\\''") + "'"
      }).join(' ')} > /tmp/svc.log 2>&1 &`])
    } catch (err) {
      pending.delete(moduleId)
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      await (box as any).stop().catch(() => {/* best-effort */})
      return { ok: false, error: `Failed to start service: ${err instanceof Error ? err.message : err}` }
    }
  }

  const healthy = await waitHealthy(hostPort, healthPath)
  pending.delete(moduleId)
  if (!healthy) {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    await (box as any).stop().catch(() => {/* best-effort */})
    return { ok: false, error: `Service did not become healthy within 60s (checked ${healthPath})` }
  }

  registry.set(moduleId, { hostPort, config: cfg, box })
  console.log(`[svc] module ${moduleId} running on port ${hostPort}`)
  return { ok: true, hostPort }
}

// H1/H5 fix: in-flight dedup — concurrent calls for same moduleId share one Promise
export function startService(moduleId: string, cfg: ContainerConfig): Promise<StartResult> {
  if (registry.has(moduleId)) return Promise.resolve({ ok: true, hostPort: registry.get(moduleId)!.hostPort })
  const existing = starting.get(moduleId)
  if (existing) return existing
  const p = doStartService(moduleId, cfg).finally(() => starting.delete(moduleId))
  starting.set(moduleId, p)
  return p
}

export async function stopService(moduleId: string): Promise<void> {
  const entry = registry.get(moduleId)
  if (!entry) return
  registry.delete(moduleId)
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  await (entry.box as any).stop().catch((err: unknown) => {
    console.warn(`[svc] stop ${moduleId}:`, err instanceof Error ? err.message : err)
  })
}

export async function stopAll(): Promise<void> {
  // M2: stop both running and pending (started but not yet healthy) containers
  const pendingStops = [...pending.entries()].map(async ([id, box]) => {
    pending.delete(id)
    await (box as { stop(): Promise<void> }).stop().catch(() => {/* best-effort */})
  })
  await Promise.all([...[...registry.keys()].map(stopService), ...pendingStops])
}

export function getHostPort(moduleId: string): number | null {
  return registry.get(moduleId)?.hostPort ?? null
}

// Returns true if a container for this moduleId is running or in-flight (starting).
export function isRegistered(moduleId: string): boolean {
  return registry.has(moduleId) || starting.has(moduleId)
}

// Proxy an HTTP request to the service container.
// Returns { status, headers, rawBody } — caller forwards headers and body as-is.
export async function proxyToService(
  moduleId: string,
  method: 'GET' | 'POST',
  subPath: string,
  query: string,
  body?: unknown,
): Promise<{ status: number; headers: Record<string, string>; rawBody: Buffer }> {
  const entry = registry.get(moduleId)
  if (!entry) throw Object.assign(new Error(`Service ${moduleId} not running`), { statusCode: 503 })

  return new Promise((resolve, reject) => {
    const payload = body ? JSON.stringify(body) : undefined
    const opts: http.RequestOptions = {
      hostname: '127.0.0.1',
      port: entry.hostPort,
      path: subPath + query,
      method,
      headers: {
        'Content-Type': 'application/json',
        ...(payload ? { 'Content-Length': Buffer.byteLength(payload) } : {}),
      },
      timeout: 30_000,
    }
    const req = http.request(opts, (res) => {
      const chunks: Buffer[] = []
      res.on('data', (c: Buffer) => chunks.push(c))
      res.on('end', () => {
        // L2: pass container headers through transparently (strip RFC 7230 hop-by-hop headers)
        const HOP_BY_HOP = new Set([
          'transfer-encoding', 'connection', 'keep-alive',
          'proxy-authenticate', 'proxy-authorization', 'te', 'trailer', 'upgrade',
        ])
        const fwd: Record<string, string> = {}
        for (const [k, v] of Object.entries(res.headers)) {
          if (!HOP_BY_HOP.has(k.toLowerCase()) && typeof v === 'string') {
            fwd[k] = v
          }
        }
        resolve({ status: res.statusCode ?? 200, headers: fwd, rawBody: Buffer.concat(chunks) })
      })
    })
    req.on('error', reject)
    req.on('timeout', () => { req.destroy(); reject(new Error(`Proxy timeout to ${moduleId}:${subPath}`)) })
    if (payload) req.write(payload)
    req.end()
  })
}
