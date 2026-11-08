// KB-SOP-01a (#1669): the daemon side of the admission, read straight from
// main (never through the renderer's proxy). One GET of
// /api/v1/sessions/{id}/source-floor, which folds the conversation's floor and
// each listed run on the daemon (decision 2026-11-09-floor-read-api, B) and
// answers one word. Anything but a 200 with exactly that word throws, which
// the admission counts as Unknown.

import http from 'node:http'
import { daemonConnection, type DaemonConnection } from './backend-manager'
import { MAX_ADMISSION_RUNS, type AdmissionReader, type FloorReading } from './knowledge-entry-admission'

const REQUEST_TIMEOUT_MS = 5_000
/** The answer is one short JSON object; anything longer is not it. */
const MAX_BODY_BYTES = 1024
const READINGS: readonly FloorReading[] = ['ordinary', 'restricted', 'unknown', 'missing']

/** One GET to the daemon: its status and body. */
export type FloorTransport = (connection: DaemonConnection, path: string) => Promise<{ status: number; body: string }>

export function floorPath(sessionId: string, runIds: readonly string[]): string {
  const query = new URLSearchParams(runIds.map((id) => ['run_id', id]))
  const tail = runIds.length > 0 ? `?${query.toString()}` : ''
  return `/api/v1/sessions/${encodeURIComponent(sessionId)}/source-floor${tail}`
}

/** The reading in a daemon answer; throws on anything else. */
export function parseFloor(response: { status: number; body: string }): FloorReading {
  if (response.status !== 200) throw new Error(`source floor: status ${response.status}`)
  const parsed: unknown = JSON.parse(response.body)
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) throw new Error('source floor: not an object')
  const keys = Object.keys(parsed)
  const value = (parsed as { classification?: unknown }).classification
  if (keys.length !== 1 || typeof value !== 'string' || !READINGS.includes(value as FloorReading)) {
    throw new Error('source floor: unexpected answer')
  }
  return value as FloorReading
}

export function daemonFloorReader(deps: { connection?: () => DaemonConnection | null; transport?: FloorTransport } = {}): AdmissionReader {
  const connection = deps.connection ?? daemonConnection
  const transport = deps.transport ?? httpTransport
  return {
    read: async (sessionId, runIds) => {
      // The admission refuses before asking; this is only a second lock.
      if (runIds.length > MAX_ADMISSION_RUNS) return 'unknown'
      const conn = connection()
      if (!conn) throw new Error('source floor: no daemon connection')
      return parseFloor(await transport(conn, floorPath(sessionId, runIds)))
    },
  }
}

/** Loopback GET to the daemon, body capped at {@link MAX_BODY_BYTES}. */
const httpTransport: FloorTransport = (connection, path) =>
  new Promise((resolve, reject) => {
    const options = { host: '127.0.0.1', port: connection.port, method: 'GET', path, headers: { ...connection.authHeaders }, timeout: REQUEST_TIMEOUT_MS }
    const req = http.request(options, (res) => {
      const chunks: Buffer[] = []
      let size = 0
      res.on('error', () => reject(new Error('source floor request failed')))
      res.on('data', (chunk: Buffer) => {
        size += chunk.length
        if (size > MAX_BODY_BYTES) {
          // Settled here: whatever the stream emits next is ignored.
          reject(new Error('source floor: answer too long'))
          res.destroy()
          return
        }
        chunks.push(chunk)
      })
      res.on('end', () => {
        if (size > MAX_BODY_BYTES) return
        resolve({ status: res.statusCode ?? 0, body: Buffer.concat(chunks).toString('utf8') })
      })
    })
    req.on('timeout', () => req.destroy(new Error('timeout')))
    req.on('error', () => reject(new Error('source floor request failed')))
    req.end()
  })

export const testing = { httpTransport }
