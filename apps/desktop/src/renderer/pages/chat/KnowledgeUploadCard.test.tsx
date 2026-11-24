// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest'
import { act, fireEvent, render, screen, within } from '@testing-library/react'
import type { KnowledgeUpload } from '../../../shared/ipc-types'
import { KnowledgeUploadCard } from './KnowledgeUploadCard'

function up(over: Partial<KnowledgeUpload> & Pick<KnowledgeUpload, 'uploadId' | 'state' | 'fileName'>): KnowledgeUpload {
  return {
    reason: null,
    jobId: null,
    enrichment: null,
    costFinal: false,
    costZero: false,
    costUnknown: false,
    originalRetained: false,
    updatedAt: 1,
    ...over,
  }
}

function row(text: string): HTMLElement {
  const el = screen.getByText(text).closest('.extract-card-value')
  if (!el) throw new Error(`no row for ${text}`)
  return el as HTMLElement
}

describe('KnowledgeUploadCard', () => {
  it('picks files and reports the new ids', async () => {
    const picked = up({ uploadId: 'new', fileName: '合同.md', state: 'sending' })
    const uploadPick = vi.fn().mockResolvedValue([picked])
    const onPicked = vi.fn()
    Object.defineProperty(window, 'agent24', {
      value: {
        knowledge: {
          uploadPick,
          uploadList: vi.fn().mockResolvedValue([]),
          uploadCancel: vi.fn(),
          onUploadChanged: vi.fn(() => () => {}),
        },
      },
      writable: true,
      configurable: true,
    })
    render(<KnowledgeUploadCard uploadIds={[]} onPicked={onPicked} />)
    expect(screen.getByText('支持 .md、.txt，单个不超过 10 MB；PDF、Word 暂不支持，选了也不会上传。')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: '上传文档' }))
    await vi.waitFor(() => expect(onPicked).toHaveBeenCalledWith(['new']))
    expect(uploadPick).toHaveBeenCalledTimes(1)
  })

  it('follows the live list and unsubscribes on unmount', async () => {
    const unsub = vi.fn()
    let push: (list: KnowledgeUpload[]) => void = () => {}
    const uploadList = vi.fn().mockResolvedValue([
      up({ uploadId: 'a', fileName: '甲.md', state: 'sending' }),
      up({ uploadId: 'hidden', fileName: '丙.md', state: 'saved' }),
    ])
    Object.defineProperty(window, 'agent24', {
      value: {
        knowledge: {
          uploadPick: vi.fn(),
          uploadList,
          uploadCancel: vi.fn(),
          onUploadChanged: vi.fn((cb: (list: KnowledgeUpload[]) => void) => { push = cb; return unsub }),
        },
      },
      writable: true,
      configurable: true,
    })
    const { unmount } = render(<KnowledgeUploadCard uploadIds={['a']} onPicked={() => {}} />)
    expect(await screen.findByText('《甲.md》正在上传')).toBeInTheDocument()
    expect(screen.queryByText('《丙.md》已保存，等待处理')).toBeNull()
    act(() => {
      push([up({ uploadId: 'a', fileName: '甲.md', state: 'processing' })])
    })
    expect(screen.getByText('《甲.md》处理中')).toBeInTheDocument()
    expect(screen.queryByText('《甲.md》正在上传')).toBeNull()
    unmount()
    expect(unsub).toHaveBeenCalled()
  })

  it('offers cancel only while saved, processing, or still summarizing', async () => {
    const uploadCancel = vi.fn().mockResolvedValue(null)
    const list = [
      up({ uploadId: 's', fileName: '甲.md', state: 'saved' }),
      up({ uploadId: 'p', fileName: '乙.md', state: 'processing' }),
      up({ uploadId: 'r', fileName: '丙.md', state: 'indexed', enrichment: 'running' }),
      up({ uploadId: 'g', fileName: '丁.md', state: 'sending' }),
      up({ uploadId: 'i', fileName: '戊.md', state: 'indexed' }),
      up({ uploadId: 'f', fileName: '己.md', state: 'failed', reason: 'parse' }),
    ]
    Object.defineProperty(window, 'agent24', {
      value: {
        knowledge: {
          uploadPick: vi.fn(),
          uploadList: vi.fn().mockResolvedValue(list),
          uploadCancel,
          onUploadChanged: vi.fn(() => () => {}),
        },
      },
      writable: true,
      configurable: true,
    })
    render(<KnowledgeUploadCard uploadIds={list.map((u) => u.uploadId)} onPicked={() => {}} />)
    expect(await screen.findByText('《甲.md》已保存，等待处理')).toBeInTheDocument()
    expect(within(row('《甲.md》已保存，等待处理')).getByRole('button', { name: '取消' })).toBeInTheDocument()
    expect(within(row('《乙.md》处理中')).getByRole('button', { name: '取消' })).toBeInTheDocument()
    expect(within(row('《丙.md》已可检索，摘要还在生成')).getByRole('button', { name: '取消' })).toBeInTheDocument()
    expect(within(row('《丁.md》正在上传')).queryByRole('button', { name: '取消' })).toBeNull()
    expect(within(row('《戊.md》已可检索')).queryByRole('button', { name: '取消' })).toBeNull()
    expect(within(row('《己.md》处理失败（parse）')).queryByRole('button', { name: '取消' })).toBeNull()
    expect(screen.getAllByRole('button', { name: '取消' })).toHaveLength(3)
    fireEvent.click(within(row('《甲.md》已保存，等待处理')).getByRole('button', { name: '取消' }))
    expect(uploadCancel).toHaveBeenCalledWith('s')
  })
})
