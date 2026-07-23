// v1 WS event hub — GET /api/v1/events (A5 mock scope: run lifecycle only).
// Contract: protocol/events.schema.json. Envelope { v:1, seq, ts, type, payload };
// seq is monotonic per connection. Browser-Origin upgrades are rejected (CSRF).

import type { Server, IncomingMessage } from 'node:http'
import type { Duplex } from 'node:stream'
import { WebSocketServer, WebSocket } from 'ws'

export interface UsageBody {
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
  cost_usd: number
}

export type V1Event =
  | { type: 'run.started'; payload: { run_id: string; session_id: string | null; schedule_id: string | null } }
  | { type: 'model.delta'; payload: { run_id: string; text: string } }
  | { type: 'run.completed'; payload: { run_id: string; output: { text: string }; usage: UsageBody } }
  | { type: 'run.failed'; payload: { run_id: string; error: { code: string; message: string } } }

export class EventsHub {
  private readonly wss = new WebSocketServer({ noServer: true })
  private readonly seqs = new Map<WebSocket, number>()

  attach(server: Server, path = '/api/v1/events'): void {
    server.on('upgrade', (req: IncomingMessage, socket: Duplex, head: Buffer) => {
      const url = new URL(req.url ?? '/', 'http://127.0.0.1')
      if (url.pathname !== path) {
        // NOTE: this hub owns the server's single 'upgrade' listener. When a
        // second WS path is added, replace this with a central upgrade
        // dispatcher instead of registering competing listeners.
        socket.write('HTTP/1.1 404 Not Found\r\n\r\n')
        socket.destroy()
        return
      }
      // Reject upgrades carrying a browser Origin header (SPEC-002 §4).
      // Presence check (not truthiness): an empty-string Origin must also be
      // rejected — browsers may send `Origin: null` for opaque origins.
      if (req.headers['origin'] !== undefined) {
        socket.write('HTTP/1.1 403 Forbidden\r\n\r\n')
        socket.destroy()
        return
      }
      this.wss.handleUpgrade(req, socket, head, (ws) => {
        this.seqs.set(ws, 0)
        ws.on('close', () => this.seqs.delete(ws))
        ws.on('error', () => this.seqs.delete(ws))
      })
    })
  }

  broadcast(ev: V1Event): void {
    const ts = new Date().toISOString()
    for (const [ws, seq] of this.seqs) {
      if (ws.readyState !== WebSocket.OPEN) continue
      this.seqs.set(ws, seq + 1)
      ws.send(JSON.stringify({ v: 1, seq, ts, type: ev.type, payload: ev.payload }))
    }
  }

  close(): void {
    for (const ws of this.seqs.keys()) ws.close()
    this.wss.close()
  }
}
