// Documenting OS desktop channel (ADR-DOC-01 D5): `window.agent24.documents.*`.
// The renderer names an operation, never a path. Main runs it through
// `agent24 documents request` (documents-cli.ts), where each operation maps
// to a fixed route under /api/v1/documents and every id is checked against
// its pattern first; it does not go through the generic `backendProxy`.

import type { CliEndpoint } from './backend-manager'
import type { ModuleEvent } from './agentear-events'
import type { DocumentsEvent, DocumentsResponse } from '../shared/ipc-types'
import { cliBinary, fail, runDocumentsCli, spawnCli, type Spawner } from './documents-cli'

const DOCUMENT_ID = /^doc_[0-7][0-9A-HJKMNP-TV-Z]{25}$/
const JOB_ID = /^job_[0-7][0-9A-HJKMNP-TV-Z]{25}$/

/** Runs one operation the renderer asked for. What is not an allowed
 * operation the CLI refuses, unsent, as 400 `invalid_request`; so does main
 * for a request that is not even JSON. */
export function documentsCall(
  req: unknown,
  endpoint: () => CliEndpoint | null,
  run: Spawner = spawnCli,
  bin: string = cliBinary(),
): Promise<DocumentsResponse> {
  try {
    if (JSON.stringify(req) === undefined) throw new Error('not JSON')
  } catch {
    return Promise.resolve(fail(400, 'invalid_request', 'not an allowed documents operation'))
  }
  const args = ['documents', 'request', '--host-stdin', '--json']
  return runDocumentsCli(args, { request: req }, endpoint, () => {}, run, bin)
}

/** The Documenting OS's event in a WS module event, or null: another
 * module's, a kind this build does not know, or one without the id the page
 * reads by. Only that id is checked: the page reads the job or the list
 * again, so the rest is never trusted. */
export function documentsEvent(event: ModuleEvent): DocumentsEvent | null {
  if (event.module !== 'documents') return null
  const { kind, payload } = event
  const has = (key: string, pattern: RegExp): boolean => {
    const v = payload[key]
    return typeof v === 'string' && pattern.test(v)
  }
  switch (kind) {
    case 'job.progress':
    case 'job.finished':
      return has('job_id', JOB_ID) ? ({ kind, payload } as DocumentsEvent) : null
    case 'document.imported':
    case 'revision.committed':
      return has('document_id', DOCUMENT_ID) ? ({ kind, payload } as DocumentsEvent) : null
    default:
      return null
  }
}
