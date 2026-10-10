// DOC-1 slice 1 (ADR-DOC-02 §7, #705): the payloads of the documents module's
// WS events. Expected values are written out here, independently of
// openapi.yaml.

import { beforeAll, describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import Ajv2020 from 'ajv/dist/2020.js'
import addFormats from 'ajv-formats'
import { parse } from 'yaml'

type Validator = ((doc: unknown) => boolean) & { errors?: unknown }
const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')
const v: Record<string, Validator> = {}
let schemas: Record<string, any>
const NAMES = ['DocumentsJobProgressEvent', 'DocumentsJobFinishedEvent', 'DocumentsDocumentImportedEvent', 'DocumentsRevisionCommittedEvent']

beforeAll(() => {
  const openapi = parse(readFileSync(join(repoRoot, 'protocol', 'openapi.yaml'), 'utf8')) as Record<string, any>
  schemas = openapi.components.schemas
  const ajv = new Ajv2020.default({ strict: false, allErrors: true })
  addFormats.default(ajv)
  for (const name of NAMES) v[name] = ajv.compile({ ...openapi, $ref: `#/components/schemas/${name}` }) as Validator
})

const ok = (schema: string, doc: unknown) => expect(v[schema](doc), JSON.stringify(v[schema].errors)).toBe(true)
const no = (schema: string, doc: unknown) => expect(v[schema](doc), JSON.stringify(doc)).toBe(false)
const JOB = 'job_01K75A0B1C2D3E4F5G6H7J8K9M'
const DOC = 'doc_01K74Z3QJ8V5N2W9RTX6YB4M00'

describe('documents event payloads', () => {
  it('job.progress: the latest progress, ids and counts only', () => {
    const p = { job_id: JOB, kind: 'import', stage: 'text', done: 3, total: 12, unit: 'page' }
    ok('DocumentsJobProgressEvent', p)
    ok('DocumentsJobProgressEvent', { ...p, total: null })
    no('DocumentsJobProgressEvent', { ...p, done: -1 })
    no('DocumentsJobProgressEvent', { ...p, stage: undefined })
    // Never a title or file name: the stream reaches every WS client.
    no('DocumentsJobProgressEvent', { ...p, title: '通告' })
  })

  it('job.finished: an end state, with an error code only where the job has one', () => {
    const f = { job_id: JOB, kind: 'import', status: 'succeeded', attempt: 1, error_code: null }
    ok('DocumentsJobFinishedEvent', { ...f, document_id: DOC, revision: 1 })
    ok('DocumentsJobFinishedEvent', { ...f, status: 'interrupted' })
    ok('DocumentsJobFinishedEvent', { ...f, status: 'failed', error_code: 'unsupported_format', attempt: 2 })
    ok('DocumentsJobFinishedEvent', { ...f, status: 'cancelled', error_code: 'cancelled' })
    ok('DocumentsJobFinishedEvent', { ...f, status: 'cancelled' })
    no('DocumentsJobFinishedEvent', { ...f, status: 'running' })
    no('DocumentsJobFinishedEvent', { ...f, status: 'failed' })
    no('DocumentsJobFinishedEvent', { ...f, status: 'failed', error_code: 'cancelled' })
    no('DocumentsJobFinishedEvent', { ...f, status: 'failed', error_code: 'made_up' })
    no('DocumentsJobFinishedEvent', { ...f, error_code: 'unsupported_format' })
    no('DocumentsJobFinishedEvent', { ...f, status: 'cancelled', error_code: 'unsupported_format' })
    no('DocumentsJobFinishedEvent', { ...f, message: 'the file /Users/x/a.pdf' })
    no('DocumentsJobFinishedEvent', { ...f, error_code: undefined })
    // Jobs a restart ends are announced too, at most 256 (ADR §7; David, 2026-10-10).
    const said = schemas.DocumentsJobFinishedEvent.description.replace(/\s+/g, ' ')
    expect(said).toMatch(/startup recovery ends \(`interrupted`, `cancelled`\) are announced once too, best effort and at most 256/)
  })

  it('document.imported: a new document at revision 1', () => {
    const d = { document_id: DOC, revision: 1, job_id: JOB }
    ok('DocumentsDocumentImportedEvent', d)
    no('DocumentsDocumentImportedEvent', { ...d, revision: 2 })
    no('DocumentsDocumentImportedEvent', { ...d, job_id: undefined })
    no('DocumentsDocumentImportedEvent', { ...d, filename: 'a.pdf' })
  })

  it('revision.committed: a later head revision', () => {
    ok('DocumentsRevisionCommittedEvent', { document_id: DOC, revision: 2 })
    ok('DocumentsRevisionCommittedEvent', { document_id: DOC, revision: 3, job_id: JOB })
    no('DocumentsRevisionCommittedEvent', { document_id: DOC, revision: 1 })
    no('DocumentsRevisionCommittedEvent', { document_id: 'doc_x', revision: 2 })
  })
})
