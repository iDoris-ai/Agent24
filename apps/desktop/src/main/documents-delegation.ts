// Service mode: the desktop is a first-party shell, and a module's write
// routes (documents import, …) need a delegation record for the module's
// current routes (agent24d middleware `module_delegation_required`, CORE-2
// §10 R1). A new module version is a new surface, so the record goes stale on
// every module upgrade. Then the desktop asks for it once, as
// `agent24 os delegate documents` does: an elevated request, so the user
// confirms it in the presence box, and the call is made again.

import { daemonConnection, daemonElevation } from './backend-manager'
import { sendElevating } from './elevated-request'

type Answer = { ok: boolean; error?: { code?: string } }

let asking: Promise<boolean> | null = null

/** One delegation request at a time; concurrent callers share its answer. */
export function delegateDocuments(send: typeof sendElevating = sendElevating): Promise<boolean> {
  asking ??= send({ method: 'POST', path: '/api/v1/os/documents/delegation' }, {}, { connection: daemonConnection, elevation: daemonElevation })
    .then((res) => res.ok, () => false)
    .finally(() => { asking = null })
  return asking
}

/** `call`, and once more after a granted delegation when the daemon asked for one. */
export async function withDocumentsDelegation<T extends Answer | null>(call: () => Promise<T>, delegate: () => Promise<boolean> = delegateDocuments): Promise<T> {
  const first = await call()
  if (!first || first.ok || first.error?.code !== 'module_delegation_required') return first
  return (await delegate()) ? call() : first
}
