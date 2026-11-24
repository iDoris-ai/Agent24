// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen } from '@testing-library/react'
import KnowledgeCitationCard, { citationLines } from './KnowledgeCitationCard'
import { CapabilitiesContext } from '../../capabilities/types'
import { setLocale } from '../../i18n'

const hit = (kb: string, doc: string, title: string) => ({ knowledge_base_id: kb, knowledge_id: doc, chunk_id: `c-${doc}`, title })

describe('L7-K1: the citation card', () => {
  it('groups by library, names bound libraries, numbers the hits of a document', () => {
    const found = [hit('kb-a', 'd1', '制度'), hit('kb-b', 'd2', '项目'), hit('kb-a', 'd1', '制度'), hit('kb-a', 'd3', '附录')]
    const lines = citationLines(found, ['kb-a', 'kb-b'], { 'kb-a': '制度库' }, '知识库 {n}', '知识库')
    expect(lines.map((l) => `${l.library}|${l.title}|${l.place}`)).toEqual(['制度库|制度|1', '制度库|制度|2', '制度库|附录|1', '知识库 2|项目|1'])
  })

  it('shows 「库名 · 文档名 · 位置」, opens the knowledge page, copies the sources and no text', async () => {
    setLocale('zh')
    const open = vi.fn()
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.assign(navigator, { clipboard: { writeText } })
    render(<CapabilitiesContext.Provider value={{ defs: [], open }}>
      <KnowledgeCitationCard found={[hit('kb-a', 'd1', '差旅制度'), hit('kb-b', 'd2', '项目手册')]} bound={['kb-a', 'kb-b']} names={{ 'kb-a': '制度库', 'kb-b': '项目库' }} />
    </CapabilitiesContext.Provider>)
    expect(screen.getByText('制度库 · 《差旅制度》 · 第 1 段命中')).toBeInTheDocument()
    expect(screen.getByText('项目库 · 《项目手册》 · 第 1 段命中')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: '到知识页查看原文' }))
    expect(open).toHaveBeenCalledWith('knowledgeBase', 'other')
    fireEvent.click(screen.getByRole('button', { name: '复制出处' }))
    expect(writeText).toHaveBeenCalledWith('出处（知识库 · 文档 · 位置）\n制度库 · 《差旅制度》 · 第 1 段命中\n项目库 · 《项目手册》 · 第 1 段命中')
    expect(await screen.findByRole('button', { name: '已复制' })).toBeInTheDocument()
  })

  it('names only bound libraries: an unbound id reads plainly, never as an id; one library shows no hint of another', () => {
    setLocale('zh')
    render(<KnowledgeCitationCard found={[hit('kb-secret', 'd1', '甲')]} bound={['kb-a']} names={{ 'kb-secret': '不该显示' }} />)
    expect(screen.getByText('知识库 · 《甲》 · 第 1 段命中')).toBeInTheDocument()
    expect(screen.queryByText(/kb-secret|不该显示/)).toBeNull()
  })

  it('renders nothing without hits', () => {
    const { container } = render(<KnowledgeCitationCard found={undefined} bound={[]} names={{}} />)
    expect(container).toBeEmptyDOMElement()
    expect(render(<KnowledgeCitationCard found={[]} bound={[]} names={{}} />).container).toBeEmptyDOMElement()
  })
})
