// Importing a file into the Documenting OS (ADR-DOC-01 D5, ADR-DOC-02 §5.6)
// runs the CLI's `agent24 documents import`: the one implementation the
// desktop and the CLI share (AGENTS.md; the agent24-documents-client crate),
// so a use case run through the CLI covers this import too. Main hands the
// CLI the daemon's address, its token and the file's path on stdin, never in
// argv, and reads its JSON lines back. The renderer never sees the path, and
// the public API only ever gets the base name (ADR-001).

import { spawn, type ChildProcess } from 'node:child_process'
import type { DocumentsError, DocumentsImportProgress, DocumentsResponse } from '../shared/ipc-types'
import { resolveBundledRustBinary, type BackendEndpoint } from './backend-manager'

type Answer = DocumentsResponse<'job'>

/** Starts the CLI with these arguments, stdin, stdout and stderr piped. */
export type Spawner = (bin: string, args: string[]) => ChildProcess

const ARGS = ['documents', 'import', '--host-stdin', '--json']
/** Far above any line the CLI writes (a job is a few KiB): past it, stop. */
const MAX_LINE_CHARS = 4 * 1024 * 1024

function fail(status: number, code: string, message: string): DocumentsResponse<never> {
  return { ok: false, status, error: { code, message } }
}

function isError(v: unknown): v is DocumentsError {
  if (typeof v !== 'object' || v === null) return false
  const e = v as Record<string, unknown>
  return typeof e['code'] === 'string' && typeof e['message'] === 'string'
}

/** `AGENT24_CLI_BIN`, or the `agent24` shipped with the app. */
export function cliBinary(): string {
  return process.env['AGENT24_CLI_BIN'] || resolveBundledRustBinary('agent24')
}

const spawnCli: Spawner = (bin, args) => spawn(bin, args, { stdio: ['pipe', 'pipe', 'pipe'] })

/** Imports the file at `path` into the daemon at `endpoint()`; on success
 * the answer is the import job. */
export function importFile(
  path: string,
  endpoint: () => BackendEndpoint | null,
  onProgress: (p: DocumentsImportProgress) => void = () => {},
  run: Spawner = spawnCli,
  bin: string = cliBinary(),
): Promise<Answer> {
  const ep = endpoint()
  if (!ep) return Promise.resolve(fail(503, 'backend_not_ready', 'Backend daemon has not announced readiness yet'))
  return new Promise((resolve) => {
    let settled = false
    let result: Answer | null = null
    let pending = ''
    const settle = (answer: Answer): void => {
      if (settled) return
      settled = true
      resolve(answer)
    }
    const unstartable = (): void => settle(fail(503, 'backend_unreachable', 'the import tool could not be started'))
    // One line of the CLI's `--json` output; anything else is ignored.
    const take = (line: string): void => {
      let parsed: unknown
      try {
        parsed = JSON.parse(line)
      } catch {
        return
      }
      if (parsed === null || typeof parsed !== 'object') return
      const { type, sent, total, ok, status, data, error } = parsed as Record<string, unknown>
      if (type === 'progress' && Number.isSafeInteger(sent) && Number.isSafeInteger(total)) {
        onProgress({ sent: sent as number, total: total as number })
      } else if (type === 'result' && typeof ok === 'boolean' && Number.isInteger(status)) {
        if (ok && data !== undefined && data !== null) result = { ok: true, status: status as number, data: data as never }
        else if (!ok && isError(error)) result = { ok: false, status: status as number, error }
        else result = fail(502, 'invalid_response', 'the import tool gave a result that does not fit')
      }
    }
    let child: ChildProcess
    try {
      child = run(bin, ARGS)
    } catch {
      unstartable()
      return
    }
    child.on('error', unstartable)
    child.stdout?.setEncoding('utf8')
    child.stdout?.on('data', (chunk: string) => {
      if (settled) return
      pending += chunk
      for (let nl = pending.indexOf('\n'); nl >= 0; nl = pending.indexOf('\n')) {
        if (nl > MAX_LINE_CHARS) break
        take(pending.slice(0, nl))
        pending = pending.slice(nl + 1)
      }
      // What is left is a line past the bound or the start of one.
      if (pending.length > MAX_LINE_CHARS) {
        pending = ''
        child.kill()
        settle(fail(502, 'invalid_response', 'the import tool wrote a line too long'))
      }
    })
    // Drained, never shown: it can name the file.
    child.stderr?.resume()
    child.on('close', () => {
      if (pending) take(pending)
      settle(result ?? fail(502, 'invalid_response', 'the import tool ended without a result'))
    })
    // The CLI may exit before reading all of it (a bad endpoint, say).
    child.stdin?.on('error', () => {})
    child.stdin?.end(JSON.stringify({ base: `http://127.0.0.1:${ep.port}`, token: ep.token, path }))
  })
}
