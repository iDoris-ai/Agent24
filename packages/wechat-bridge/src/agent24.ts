// Client for the local agent24d v1 HTTP API.
// Transport only: one finished WeChat message becomes one inbound event.
// The daemon admits it. This client does not start a run or decide an approval.

import { CONFIG, type DaemonEndpoint } from './config.js'

export interface InboundRecord {
  id: string
  outcome: 'duplicate' | 'ignored' | 'started' | 'already_linked'
  reason?: string
  run_id?: string
}

export interface InboundEvent {
  id: string
  actor: string
  at: string
  text?: string
}

export class Agent24Client {
  constructor(private readonly ep: DaemonEndpoint) {}

  private headers(): Record<string, string> {
    // Last line of defence: no bearer, no request (every method builds headers before fetch).
    if (!this.ep.token) throw new Error('agent24d bearer token missing: refusing to send an unauthenticated request')
    const h: Record<string, string> = { 'Content-Type': 'application/json', Authorization: `Bearer ${this.ep.token}` }
    return h
  }

  /** POST one event. The route fixes `source` to wechat; this body does not send one. */
  async recordInbound(event: InboundEvent): Promise<InboundRecord> {
    const res = await fetch(`${this.ep.base}${CONFIG.INBOUND_EVENTS_PATH}`, {
      method: 'POST',
      headers: this.headers(),
      body: JSON.stringify(event),
    })
    if (!res.ok) throw new Error(`record inbound failed: HTTP ${res.status}`)
    return (await res.json()) as InboundRecord
  }
}
