import { describe, expect, it } from 'vitest'
import { SAVE_MARK, asksRestoreVersion, asksSaveToOriginal, restoreOffer, savesToOriginal } from './doc-version'
import { docAgentPrompt } from './doc-intent'

describe('L6-F5: saved into the original only when the user says so', () => {
  it('hears 「存到原文件」 and the like', () => {
    for (const s of [
      '把第 3 段改正式一点，直接存到原文件里',
      '把第三条改成按月付款，存回原文件',
      '改完直接保存到原文件',
      '直接改原文件就行',
      '直接在原文件上改',
      '帮我把甲方改成上海明远，覆盖原文件',
      '改好以后存成这份文档的新版本',
      '保存为新版本',
      '不用另存，直接存原文件',
      '就存在原来的文件里',
    ]) expect(asksSaveToOriginal(s), s).toBe(true)
  })

  it('does not take other saving for it', () => {
    for (const s of [
      '存一下这段话',
      '保存成 PDF',
      '另存一份',
      '把第 3 段改正式一点',
      '把第 3 段改正式一点，另存为新文档',
      '不要改原文件',
      '别存到原文件，另存一份',
      '原文件不要动',
      '原文件在哪里？',
      '保存到下载',
      '把这份存成 Word',
      '这个版本号是多少',
      '新版本的合同什么时候生效',
    ]) expect(asksSaveToOriginal(s), s).toBe(false)
  })

  it('marks the turn the App judged, and only that one', () => {
    const doc = { id: 'doc_01K74Z3QJ8V5N2W9RTX6YB4MCD', title: '合同.docx' }
    const asked = docAgentPrompt(doc, '把第 3 段改正式一点，直接存到原文件里')
    expect(savesToOriginal(asked)).toBe(true)
    expect(asked).toContain('save_to')
    const not = docAgentPrompt(doc, '把第 3 段改正式一点')
    expect(savesToOriginal(not)).toBe(false)
    expect(not).not.toContain('save_to')
    // The user's own words are never the mark.
    expect(savesToOriginal('直接存到原文件里')).toBe(false)
    expect(SAVE_MARK.length).toBeGreaterThan(8)
  })
})

describe('L6-F5: 「回到上一个版本」 is the App\'s, never the model\'s', () => {
  it('hears going back to a version', () => {
    expect(asksRestoreVersion('回到上一个版本')).toEqual({ to: 'previous' })
    expect(asksRestoreVersion('恢复到上一版')).toEqual({ to: 'previous' })
    expect(asksRestoreVersion('帮我退回之前的版本')).toEqual({ to: 'previous' })
    expect(asksRestoreVersion('还原成第 1 版')).toEqual({ to: 1 })
    expect(asksRestoreVersion('恢复第二版')).toEqual({ to: 2 })
    expect(asksRestoreVersion('换回原来的版本吧')).toEqual({ to: 'previous' })
  })

  it('leaves other sentences alone', () => {
    for (const s of ['上一个版本改了什么？', '恢复出厂设置', '这一版和上一版有什么不同', '回到第 3 页', '不要回到上一个版本', '版本号是多少']) {
      expect(asksRestoreVersion(s), s).toBeNull()
    }
  })

  it('offers the version before the current one, or the one named', () => {
    const versions = { document_id: 'doc_x', head_revision: 3, revisions: [
      { revision: 3, created_at: '2026-11-23T03:00:00Z' }, { revision: 2, created_at: '2026-11-23T02:00:00Z' }, { revision: 1, created_at: '2026-11-22T01:00:00Z' },
    ] }
    expect(restoreOffer(versions, { to: 'previous' })).toMatchObject({ kind: 'offer', from: 2, head: 3 })
    expect(restoreOffer(versions, { to: 1 })).toMatchObject({ kind: 'offer', from: 1, head: 3 })
    expect(restoreOffer(versions, { to: 3 })).toMatchObject({ kind: 'none' })
    expect(restoreOffer(versions, { to: 7 })).toMatchObject({ kind: 'none' })
    const one = { document_id: 'doc_x', head_revision: 1, revisions: [{ revision: 1, created_at: '2026-11-22T01:00:00Z' }] }
    expect(restoreOffer(one, { to: 'previous' })).toMatchObject({ kind: 'none' })
  })
})
