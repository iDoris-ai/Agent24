import { describe, it, expect, vi, beforeAll, afterAll } from 'vitest'
import http from 'node:http'

// Start the server in-process and query it via http
// Use a random port to avoid conflicts
let server: http.Server | null = null
const PORT = 8765

beforeAll(async () => {
  // Patch process.env so LLMGateway doesn't try real oMLX
  process.env['OMLX_URL'] = 'http://127.0.0.1:19999'
  process.env['OMLX_API_KEY'] = 'test'

  // Dynamic import AFTER env patch so module picks up the values
  const mod = await import('./server')
  // server.ts calls start() at module load — we need to intercept
  // Instead, we test via HTTP against the running server
  // But server.ts auto-starts on import. Use a different approach:
  // We'll test the daemon indirectly by spinning up a fresh instance.
  void mod // suppress unused warning
}, 10000)

afterAll(() => {
  server?.close()
})

function request(method: string, path: string, body?: unknown): Promise<{ status: number; data: unknown }> {
  return new Promise((resolve, reject) => {
    const payload = body ? JSON.stringify(body) : undefined
    const req = http.request(
      {
        host: '127.0.0.1',
        port: PORT,
        path,
        method,
        headers: {
          'Content-Type': 'application/json',
          ...(payload ? { 'Content-Length': Buffer.byteLength(payload) } : {}),
        },
      },
      (res) => {
        const chunks: Buffer[] = []
        res.on('data', (c: Buffer) => chunks.push(c))
        res.on('end', () => {
          try {
            resolve({ status: res.statusCode ?? 500, data: JSON.parse(Buffer.concat(chunks).toString()) })
          } catch {
            resolve({ status: res.statusCode ?? 500, data: null })
          }
        })
      },
    )
    req.on('error', reject)
    if (payload) req.write(payload)
    req.end()
  })
}

describe('daemon server routes', () => {
  it('GET /health returns status ok', async () => {
    const { status, data } = await request('GET', '/health')
    expect(status).toBe(200)
    expect((data as { status: string }).status).toBe('ok')
    expect(typeof (data as { ts: number }).ts).toBe('number')
  })

  it('GET /api/modules returns array of module manifests', async () => {
    const { status, data } = await request('GET', '/api/modules')
    expect(status).toBe(200)
    expect(Array.isArray(data)).toBe(true)
    const manifests = data as Array<{ id: string; type: string }>
    expect(manifests.length).toBeGreaterThan(0)
    expect(manifests.every(m => m.id && m.type)).toBe(true)
  })

  it('GET /api/llm/usage returns array', async () => {
    const { status, data } = await request('GET', '/api/llm/usage')
    expect(status).toBe(200)
    expect(Array.isArray(data)).toBe(true)
  })

  it('POST /api/llm/chat missing messages returns 400', async () => {
    const { status } = await request('POST', '/api/llm/chat', {})
    expect(status).toBe(400)
  })

  it('GET /unknown-path returns 404', async () => {
    const { status } = await request('GET', '/no-such-route')
    expect(status).toBe(404)
  })

  it('GET /api/capabilities/ping returns ok', async () => {
    const { status, data } = await request('GET', '/api/capabilities/ping')
    expect(status).toBe(200)
    expect((data as { status: string }).status).toBe('ok')
  })
})
