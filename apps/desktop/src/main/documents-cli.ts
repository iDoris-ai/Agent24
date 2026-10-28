// Main runs the CLI's `agent24 documents …` for the Documenting OS
// (ADR-DOC-01 D5): the one implementation the desktop and the CLI share
// (AGENTS.md; the agent24-documents-client crate), so a use case run through
// the CLI covers the desktop too. The daemon's address and token, and what
// to run on (a file's path, an operation), go on stdin, never in argv; the
// CLI's JSON lines come back on stdout.

import { spawn, type ChildProcess } from 'node:child_process'
import type { DocumentsError, DocumentsImportProgress, DocumentsResponse } from '../shared/ipc-types'
import { resolveBundledRustBinary, type CliEndpoint } from './backend-manager'

/** Starts the CLI with these arguments, stdin, stdout and stderr piped. */
export type Spawner = (bin: string, args: string[]) => ChildProcess

/** Far above any line the CLI writes (a reply is at most 2 MiB, the
 * transport's cap): past it, stop. */
const MAX_LINE_CHARS = 4 * 1024 * 1024
/** How long a CLI that printed its result may take to exit; past it, it is
 * stopped and the result stands. */
export const RESULT_GRACE_MS = 5_000

export function fail(status: number, code: string, message: string): DocumentsResponse<never> {
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

export const spawnCli: Spawner = (bin, args) => spawn(bin, args, { stdio: ['pipe', 'pipe', 'pipe'] })

/** Runs `agent24 <args>` (a `--host-stdin --json` command) for the daemon
 * at `endpoint()`: stdin gets `{"base","token",...input}`, `progress` lines
 * go to `onProgress`, and the answer is the `result` line. */
export function runDocumentsCli(
  args: string[],
  input: Record<string, unknown>,
  endpoint: () => CliEndpoint | null,
  onProgress: (p: DocumentsImportProgress) => void = () => {},
  run: Spawner = spawnCli,
  bin: string = cliBinary(),
): Promise<DocumentsResponse> {
  const ep = endpoint()
  if (!ep) return Promise.resolve(fail(503, 'backend_not_ready', 'Backend daemon has not announced readiness yet'))
  return new Promise((resolve) => {
    let settled = false
    let result: DocumentsResponse | null = null
    let pending = ''
    let grace: ReturnType<typeof setTimeout> | undefined
    const settle = (answer: DocumentsResponse): void => {
      if (settled) return
      settled = true
      clearTimeout(grace)
      resolve(answer)
    }
    // The result is in: a CLI that does not then exit is stopped, so the
    // renderer's request never hangs on it.
    const answered = (answer: DocumentsResponse): void => {
      result = answer
      grace ??= setTimeout(() => {
        child.kill()
        settle(answer)
      }, RESULT_GRACE_MS)
    }
    const unstartable = (): void => settle(fail(503, 'backend_unreachable', 'the documents tool could not be started'))
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
        // A throwing listener must not lose the result line still to come.
        try {
          onProgress({ sent: sent as number, total: total as number })
        } catch {
          /* the next progress line tries again */
        }
      } else if (type === 'result' && typeof ok === 'boolean' && Number.isInteger(status)) {
        const code = status as number
        // A resource is a JSON object with a 2xx. An error keeps the CLI's
        // status: a reply that broke the protocol is `invalid_response` with
        // the status the daemon gave (a 302, a 200 that is not JSON).
        if (ok && code >= 200 && code < 300 && typeof data === 'object' && data !== null) {
          answered({ ok: true, status: code, data: data as never })
        } else if (!ok && isError(error)) answered({ ok: false, status: code, error })
        else answered(fail(502, 'invalid_response', 'the documents tool gave a result that does not fit'))
      }
    }
    let child: ChildProcess
    try {
      child = run(bin, args)
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
        settle(fail(502, 'invalid_response', 'the documents tool wrote a line too long'))
      }
    })
    // Drained, never shown: it can name a file.
    child.stderr?.resume()
    child.on('close', () => {
      if (pending) take(pending)
      settle(result ?? fail(502, 'invalid_response', 'the documents tool ended without a result'))
    })
    // The CLI may exit before reading all of it (a bad endpoint, say).
    child.stdin?.on('error', () => {})
    child.stdin?.end(JSON.stringify({ base: `http://127.0.0.1:${ep.port}`, token: ep.token, ...input }))
  })
}
