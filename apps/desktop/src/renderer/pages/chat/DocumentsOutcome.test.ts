import { describe, expect, it, vi } from 'vitest'
import { jobOutcome, runDocumentsOf } from './DocumentsOutcome'

describe('runDocumentsOf', () => {
  it('reads the daemon\'s record, nothing else', () => {
    expect(runDocumentsOf({ attempted: true, jobs: ['job_1', 2] })).toEqual({ attempted: true, jobs: ['job_1'], failed: [] })
    expect(runDocumentsOf({ attempted: true, jobs: ['job_1'], failed: ['doc_01B', 3] })).toEqual({ attempted: true, jobs: ['job_1'], failed: ['doc_01B'] })
    expect(runDocumentsOf(undefined)).toBeNull()
    expect(runDocumentsOf({ attempted: false, jobs: [] })).toBeNull()
    // L6-F5: a save into the original, no job.
    expect(runDocumentsOf({ attempted: true, jobs: [], failed: [], revisions: [{ document_id: 'doc_1', title: '合同.docx', revision: 2 }, { revision: 'x' }] }))
      .toEqual({ attempted: true, jobs: [], failed: [], revisions: [{ document_id: 'doc_1', title: '合同.docx', revision: 2 }] })
  })
})

describe('jobOutcome', () => {
  it('waits for the import and names what it made', async () => {
    const request = vi.fn()
      .mockResolvedValueOnce({ ok: true, data: { job_id: 'j', status: 'running' } })
      .mockResolvedValueOnce({ ok: true, data: { job_id: 'j', status: 'succeeded', result: { document_id: 'doc_1' } } })
      .mockResolvedValueOnce({ ok: true, data: { document_id: 'doc_1', title: '合同（修改）' } })
    expect(await jobOutcome({ request } as never, 'j', async () => {})).toEqual({ kind: 'made', title: '合同（修改）' })
  })
  it('a failed import made nothing', async () => {
    const request = vi.fn().mockResolvedValue({ ok: true, data: { job_id: 'j', status: 'failed' } })
    expect(await jobOutcome({ request } as never, 'j', async () => {})).toEqual({ kind: 'failed' })
  })
})
