// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import DocumentsPage from './Documents'
import { resetImportStore } from './importStore'
import { setLocale } from '../../i18n'

const doc = {
  document_id: 'doc_01K74Z3QJ8V5N2W9RTX6YB4M01',
  title: '租赁合同',
  media_type: 'application/pdf',
  head_revision: 1,
  created_at: '2026-10-08T09:00:00.000Z',
  updated_at: '2026-10-08T09:00:00.000Z',
}
const detail = {
  ...doc,
  head: { revision: 2, content_sha256: `sha256:${'a'.repeat(64)}`, size: 2048, media_type: 'application/pdf' },
}
type Res = { ok: boolean; status: number; data?: unknown; error?: unknown }
const ok = (data: unknown): Promise<Res> => Promise.resolve({ ok: true, status: 200, data })
const err = (status: number, code: string, message: string): Promise<Res> =>
  Promise.resolve({ ok: false, status, error: { code, message } })

const TEXT = '付款期限为二〇二六年十二月三十一日。'
const textPage = {
  document_id: doc.document_id,
  revision: 2,
  content_sha256: `sha256:${'a'.repeat(64)}`,
  media_type: 'application/pdf',
  text_layer_sha256: `sha256:${'c'.repeat(64)}`,
  engine: { name: 'pdfkit', version: '1' },
  parse_status: 'complete',
  unparsed_regions: [],
  blocks: [{ block_id: 'blk-pay', text: TEXT, text_sha256: `sha256:${'b'.repeat(64)}`, page: 2 }],
  next_cursor: null,
}

beforeEach(() => {
  resetImportStore()
  setLocale('zh')
  Element.prototype.scrollIntoView = vi.fn()
})

describe('DocumentsPage: summarize and check', () => {
  it('shows a cited result and jumps to page 2 from its source link', async () => {
    const request = vi.fn((req: { op: string; page?: number }) => {
      if (req.op === 'capabilities') return ok({ storage: { state: 'ready' } })
      if (req.op === 'list') return ok({ documents: [doc], next_cursor: null })
      if (req.op === 'get') return ok(detail)
      if (req.op === 'readText') return ok(textPage)
      if (req.op === 'renderPage') {
        const page = req.page ?? 1
        return ok({ media_type: 'image/png', bytes: 3, scale: 2, png: page === 2 ? 'UDI=' : 'UDE=' })
      }
      return err(400, 'invalid_request', 'unexpected')
    })
    let asks = 0
    const backendProxy = vi.fn(async () => {
      asks += 1
      if (asks === 1) {
        return {
          ok: true,
          status: 200,
          data: {
            message: {
              content: JSON.stringify([{
                kind: 'deadline',
                text: '付款期限为二〇二六年十二月三十一日',
                quote: '付款期限为二〇二六年十二月三十一日',
                ref: 'b1',
              }]),
            },
          },
        }
      }
      return {
        ok: true,
        status: 200,
        data: {
          message: {
            content: JSON.stringify({
              summary: [{ text: '合同约定付款期限', refs: [1] }],
              conflicts: [],
              missing: [],
            }),
          },
        },
      }
    })
    window.agent24 = {
      documents: {
        request,
        importFile: vi.fn(() => Promise.resolve(null)),
        onImportProgress: () => () => {},
        onEvent: () => () => {},
        installOfficeEngine: vi.fn(),
        onOfficeEngineChanged: () => () => {},
      },
      backendProxy,
    } as never

    render(<DocumentsPage />)
    fireEvent.click(await screen.findByRole('button', { name: '租赁合同' }))
    expect(await screen.findByText(TEXT)).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: '总结并检查' }))
    expect(await screen.findByText('合同约定付款期限')).toBeInTheDocument()
    fireEvent.click(screen.getAllByRole('button', { name: '第 2 页' })[0])
    await waitFor(() => {
      expect(request.mock.calls.some((c) => c[0].op === 'renderPage' && c[0].page === 2)).toBe(true)
    })
  })
})
