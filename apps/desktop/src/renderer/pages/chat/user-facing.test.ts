import { describe, expect, it } from 'vitest'
import { failureKind, withoutToolNames } from './user-facing'

describe('withoutToolNames (M3)', () => {
  it('says the tools a model named in the user\'s words', () => {
    // As a 4-bit model wrote on 0.6.70 after a refused split.
    expect(withoutToolNames('1. 检查文档是否存在：通过 `documents.get("《房屋租赁合同-测试.pdf》")` 确认文档可访问性'))
      .toBe('1. 检查文档是否存在：通过 查看文档 确认文档可访问性')
    expect(withoutToolNames('先用 documents.paragraphs 看段落，再用 documents.patch 改写；也可以 knowledge_search。'))
      .toBe('先用 查看段落 看段落，再用 改写段落 改写；也可以 检索知识库。')
    expect(withoutToolNames('我会用 fs_write 保存')).toBe('我会用 写入文件 保存')
    expect(withoutToolNames('documents.extract 读出全文')).toBe('文档操作 读出全文')
  })
  it('leaves ordinary text, file names and English words alone', () => {
    for (const text of ['请 explore 一下这个想法', '文件 documents.pdf 在下载里', 'the knowledge base', '烧开水约需 5 分钟']) {
      expect(withoutToolNames(text)).toBe(text)
    }
  })
})

describe('failureKind (M3: no run_cancelled on screen)', () => {
  it('maps codes to what the user is told', () => {
    expect(failureKind('run_cancelled')).toBe('cancelled')
    expect(failureKind('cancelled')).toBe('cancelled')
    expect(failureKind('run_timeout')).toBe('timeout')
    expect(failureKind('run_not_started')).toBe('notStarted')
    expect(failureKind('provider_unavailable')).toBe('model')
    expect(failureKind('something_else')).toBe('other')
  })
})
