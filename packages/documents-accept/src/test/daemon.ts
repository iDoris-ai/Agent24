// A stand-in for a daemon through a whole run: every route the checks use,
// answering as the contract says, with a few ways to go wrong.

import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { type Sample, loadAsk, loadSample } from '../samples'
import { type Call, anchor, answer, block, document, extracted, extraction, found, id, job, refuse, textPage, upload } from './fake'

const S01 = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..', '..', 'docs', 'documenting', 'samples', 's01')
export const NOTICE = loadSample(path.join(S01, 's01-07-mixed-building-notice')) // one page
export const HOLIDAYS = loadSample(path.join(S01, 's01-03-zh-holiday-2026'))
export const IMPORT = id('job', 9)
export const JOB = id('job')
export const RERUN = id('job', 2)
/** A PNG as far as its header chunk. */
export const PICTURE = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52, 0, 0, 2, 100, 0, 0, 3, 24]

export const capabilities = (extract = true) => ({
  os_version: '0.1.0',
  storage: { state: 'ready' },
  engines: [
    { id: 'apple-pdfkit', kind: 'parse', formats: [], state: 'ready', version: '15.2.0' },
    { id: 'apple-pdfkit', kind: 'render', formats: [], state: 'ready', version: '15.2.0' },
  ],
  knowledge: { setting: 'on', state: 'unavailable' },
  operations: [{ op: 'find', available: true, reason: null }, { op: 'extract', available: extract, reason: extract ? null : 'engine_unavailable' }],
})

export interface Odd {
  capabilities?: () => Response
  imported?: object
  text?: () => Response
  find?: () => Response
  start?: () => Response
  retry?: () => Response
  /** The sample whose job goes on running until it is cancelled. Every other job has succeeded by the time anyone looks. */
  slow?: string
  /** How the worker of the slow job takes a cancel: the cancel answers `cancelling`, and the job stops (or, `stuck`, does not). Without it the job is cancelled at once. */
  running?: 'stops' | 'stuck'
  /** How the extraction job ends, and the values it then has. */
  ends?: object
  values?: object[]
}

/** A daemon that does all a run asks, one document at a time, as the routes
 *  would: the last upload is the document, and its extraction is one job. */
export function daemon(odd: Odd = {}) {
  let doc = 0
  let sample: Sample | undefined
  let size = 0
  let state: { status?: string } = {}
  let started = false
  const content = () => `sha256:${sample?.meta.sha256_stored}`
  return (call: Call): Response => {
    const { method, route } = call
    const DOC = id('doc', doc)
    if (route === '/capabilities') return odd.capabilities?.() ?? answer(capabilities())
    if (route === '/uploads') {
      const asked = call.json as { total_size: number; sha256: string; filename: string }
      sample = [NOTICE, HOLIDAYS].find((s) => `sha256:${s.meta.sha256_stored}` === asked.sha256)
      size = asked.total_size
      doc += 1
      state = odd.slow === sample?.id ? job({ status: 'running', attempt: 1 }) : extracted({ attempt: 1 })
      started = false
      return answer(upload({ total_size: size, sha256: asked.sha256 }), 201)
    }
    if (route.endsWith('/chunks')) return answer(upload({ total_size: size, sha256: content(), received: size, status: 'complete' }))
    if (route === '/imports') return answer(job({ job_id: IMPORT, kind: 'import' }), 202)
    if (route === `/jobs/${IMPORT}`) return answer(odd.imported ?? job({ job_id: IMPORT, kind: 'import', status: 'succeeded', result: { document_id: DOC, revision: 1 } }))
    const stored = document({ document_id: DOC, head: { revision: 1, content_sha256: content(), size, media_type: 'application/pdf' } })
    if (route === `/documents/${DOC}`) return answer(stored)
    if (route === '/documents') return answer({ documents: [stored], next_cursor: null })
    if (route.endsWith('/text')) return odd.text?.() ?? answer(textPage([block('p1/b1')], null, { document_id: DOC }))
    if (route.includes('/pages/')) {
      const page = Number(route.match(/\/pages\/(\d+)\?/)?.[1])
      if (page > (sample?.meta.pages ?? 0)) return refuse(404, 'not_found')
      return new Response(new Uint8Array(PICTURE), { headers: { 'content-type': 'image/png', 'documents-render-scale': '1' } })
    }
    if (route.endsWith('/find')) return odd.find?.() ?? answer(found([anchor(1, 'p1/b1', 0, 4, { document_id: DOC })]))
    if (route === `/documents/${DOC}/extractions`) {
      if (call.headers['idempotency-key'] === undefined) {
        // The first request is the job, new; every later one is a replay of it as it now is.
        const replay = started
        started = true
        return odd.start?.() ?? (replay ? answer(state, state.status === 'succeeded' || state.status === 'cancelled' ? 200 : 202) : answer(job({ status: 'queued', attempt: 1 }), 202))
      }
      const fields = (call.json as { schema: { fields: { type?: string }[] } }).schema.fields
      const first = loadAsk(sample!)
      return fields.at(-1)?.type === first.at(-1)?.type ? answer(job({ job_id: RERUN }), 202) : refuse(422, 'idempotency_key_reused')
    }
    if (route === `/jobs/${RERUN}/cancel`) return answer(job({ job_id: RERUN, status: 'cancelled' }))
    if (route === `/jobs/${JOB}/cancel`) {
      // A job that has ended is left as it is; one that runs is stopped.
      if (state.status !== 'running') return answer(state)
      state = job({ status: odd.running === 'stuck' ? 'cancelling' : 'cancelled', attempt: 1 })
      return answer(odd.running ? job({ status: 'cancelling', attempt: 1 }) : state)
    }
    if (route === `/jobs/${JOB}/retry` && odd.retry) return odd.retry()
    if (route === `/jobs/${JOB}/retry`) {
      state = odd.ends ?? extracted({ attempt: 2 })
      return answer(job({ status: 'queued', attempt: 2 }))
    }
    if (route === `/jobs/${JOB}`) return answer(state.status === 'succeeded' ? (odd.ends ?? state) : state)
    if (method === 'GET' && route === `/extractions/${id('ext')}`) {
      const keys = loadAsk(sample!).map((f) => f.key)
      return answer(extraction(keys, null, { document_id: DOC, content_sha256: content(), ...(odd.values ? { values: odd.values } : {}) }))
    }
    return refuse(404, 'not_found')
  }
}
