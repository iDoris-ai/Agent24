// Importing a file into the Documenting OS (ADR-DOC-01 D5, ADR-DOC-02 §5.6)
// runs `agent24 documents import` (documents-cli.ts). The renderer never
// sees the path, and the public API only ever gets the base name (ADR-001).

import type { DocumentsImportProgress, DocumentsResponse } from '../shared/ipc-types'
import type { CliEndpoint } from './backend-manager'
import { cliBinary, runDocumentsCli, spawnCli, type Spawner } from './documents-cli'

/** Imports the file at `path` into the daemon at `endpoint()`; on success
 * the answer is the import job. */
export function importFile(
  path: string,
  endpoint: () => CliEndpoint | null,
  onProgress: (p: DocumentsImportProgress) => void = () => {},
  run: Spawner = spawnCli,
  bin: string = cliBinary(),
): Promise<DocumentsResponse<'job'>> {
  const args = ['documents', 'import', '--host-stdin', '--json']
  return runDocumentsCli(args, { path }, endpoint, onProgress, run, bin) as Promise<DocumentsResponse<'job'>>
}
