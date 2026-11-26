import { describe, expect, it, vi } from 'vitest'
import { asksFillForm } from './doc-intent'
import { chooseReply, formNameOf, namedTitles, pdfReply, resolveFormEntry, type FormEntryInput } from './form-entry'

type Doc = { document_id: string; title: string; media_type: string; head_revision: number; created_at: string }
const DOCX = 'application/vnd.openxmlformats-officedocument.wordprocessingml.document'
const docx = (id: string, title: string): Doc => ({ document_id: id, title, media_type: DOCX, head_revision: 1, created_at: '2026-11-26T10:00:00Z' })
const pdf = (id: string, title: string): Doc => ({ document_id: id, title, media_type: 'application/pdf', head_revision: 1, created_at: '2026-11-26T10:00:00Z' })

/** A fake documents api: `fields` has the field names per document id. */
function fakeApi(docs: Doc[], fields: Record<string, string[]>, listOk = true) {
  const request = vi.fn(async (req: { op: string; documentId?: string }) => {
    if (req.op === 'list') return listOk ? { ok: true, data: { documents: docs } } : { ok: false, error: { code: 'x', message: 'x' } }
    if (req.op === 'fields') {
      const names = fields[req.documentId ?? '']
      if (!names) return { ok: false, error: { code: 'unsupported_media_type', message: 'not docx' } }
      return { ok: true, data: { document_id: req.documentId, revision: 1, fields: names.map((name) => ({ name, count: 1 })) } }
    }
    throw new Error(`unexpected op ${req.op}`)
  })
  return { api: { request } as unknown as Window['agent24']['documents'], request }
}

const input = (text: string, over: Partial<FormEntryInput> = {}): FormEntryInput =>
  ({ doc: null, text, earlier: [], asks: true, remembered: null, ...over })

const LIB = [
  docx('doc_info', '个人信息.docx'),
  docx('doc_form', '入职登记表.docx'),
  pdf('doc_lease', '租赁合同.pdf'),
  docx('doc_plain', '会议纪要.docx'),
]
const FIELDS = { doc_form: ['姓名', '部门', '入职日期'], doc_info: [], doc_plain: [] }

describe('L9-1b: the form a turn names in words', () => {
  it('reads the form before 表 / 模板, without 这张 or what is in 《》', () => {
    expect(formNameOf('把这张入职登记表填一下，资料在《个人信息.docx》里')).toBe('入职登记表')
    expect(formNameOf('按报销单模板填一下')).toBe('报销单')
    expect(formNameOf('帮我填一下请假申请表')).toBe('请假申请表')
  })
  it('names nothing for 「这张表」「表格」 alone, or a title in 《》', () => {
    expect(formNameOf('这张表能帮我填吗')).toBeNull()
    expect(formNameOf('把表格填一下')).toBeNull()
    expect(formNameOf('《租赁合同.pdf》这张表能帮我填吗')).toBeNull()
  })
  it('lists the titles in 《》 once each', () => {
    expect(namedTitles('按《A.docx》填，资料在《B.docx》里，还是《A.docx》')).toEqual(['A.docx', 'B.docx'])
  })
})

describe('L9-1b FU-253: asking to fill a PDF is answered by the App', () => {
  it('takes 「《租赁合同.pdf》这张表能帮我填吗」 for a form turn', () => {
    expect(asksFillForm('《租赁合同.pdf》这张表能帮我填吗', false)).toBe(true)
  })
  it('says a named PDF is not supported, without the model', async () => {
    const { api } = fakeApi(LIB, FIELDS)
    expect(await resolveFormEntry(api, input('《租赁合同.pdf》这张表能帮我填吗'))).toEqual({ kind: 'pdf', title: '租赁合同.pdf' })
    const said = pdfReply('租赁合同.pdf')
    expect(said).toContain('Word')
    expect(said).toContain('打印')
    expect(said).not.toMatch(/\{\{乙方/)
  })
  it('says the same in a PDF document\'s own conversation', async () => {
    const { api } = fakeApi(LIB, FIELDS)
    const doc = { id: 'doc_lease', title: '租赁合同.pdf', revision: 1 }
    expect(await resolveFormEntry(api, input('这张表能帮我填吗', { doc }))).toEqual({ kind: 'pdf', title: '租赁合同.pdf' })
  })
})

describe('L9-1b FU-252: the template is told apart from the material', () => {
  it('finds the only 「入职登记表」 with fields when only the material is named', async () => {
    const { api } = fakeApi(LIB, FIELDS)
    const out = await resolveFormEntry(api, input('把这张入职登记表填一下，资料在《个人信息.docx》里'))
    expect(out).toEqual({
      kind: 'fill',
      template: expect.objectContaining({ id: 'doc_form', title: '入职登记表.docx' }),
      material: expect.objectContaining({ id: 'doc_info', title: '个人信息.docx' }),
    })
  })
  it('asks which one when two templates have the form\'s name, and lists them', async () => {
    const lib = [...LIB, docx('doc_form2', '入职登记表（2026版）.docx')]
    const { api } = fakeApi(lib, { ...FIELDS, doc_form2: ['姓名'] })
    const out = await resolveFormEntry(api, input('把这张入职登记表填一下，资料在《个人信息.docx》里'))
    expect(out).toEqual({ kind: 'choose', formName: '入职登记表', candidates: ['入职登记表.docx', '入职登记表（2026版）.docx'] })
    expect(chooseReply('入职登记表', ['入职登记表.docx', '入职登记表（2026版）.docx'])).toMatch(/^要填的是哪一份？.*《入职登记表\.docx》、《入职登记表（2026版）\.docx》/)
  })
  it('asks when no document with the form\'s name has fields', async () => {
    const lib = [docx('doc_info', '个人信息.docx'), docx('doc_form', '入职登记表.docx')]
    const { api } = fakeApi(lib, { doc_info: [], doc_form: [] })
    expect(await resolveFormEntry(api, input('把这张入职登记表填一下，资料在《个人信息.docx》里')))
      .toEqual({ kind: 'choose', formName: '入职登记表', candidates: [] })
    expect(chooseReply('入职登记表', [])).toMatch(/^要填的是哪一份？/)
  })
  it('never takes a title-matching PDF as a template candidate', async () => {
    const lib = [...LIB, pdf('doc_formpdf', '入职登记表.pdf')]
    const { api, request } = fakeApi(lib, FIELDS)
    const out = await resolveFormEntry(api, input('把这张入职登记表填一下，资料在《个人信息.docx》里'))
    expect(out).toMatchObject({ kind: 'fill', template: { id: 'doc_form' } })
    expect(request.mock.calls.some(([r]) => r.op === 'fields' && r.documentId === 'doc_formpdf')).toBe(false)
  })
  it('takes the named document with fields as the template, the other as material, whatever the order', async () => {
    const { api } = fakeApi(LIB, FIELDS)
    const out = await resolveFormEntry(api, input('用《个人信息.docx》把《入职登记表.docx》填一下'))
    expect(out).toMatchObject({ kind: 'fill', template: { id: 'doc_form' }, material: { id: 'doc_info' } })
  })
  it('uses this conversation\'s document as the template', async () => {
    const { api } = fakeApi(LIB, FIELDS)
    const doc = { id: 'doc_form', title: '入职登记表.docx', revision: 1 }
    const out = await resolveFormEntry(api, input('把这张表填一下，资料在《个人信息.docx》里', { doc }))
    expect(out).toMatchObject({ kind: 'fill', template: { id: 'doc_form' }, material: { id: 'doc_info' } })
  })
  it('keeps the remembered template on an answer turn, not on a new ask', async () => {
    const { api } = fakeApi(LIB, FIELDS)
    const remembered = { id: 'doc_form', title: '入职登记表.docx', revision: 1 }
    const earlier = ['把这张入职登记表填一下，资料在《个人信息.docx》里']
    expect(await resolveFormEntry(api, input('电话是 13900000000，紧急联系人先空着', { asks: false, earlier, remembered })))
      .toMatchObject({ kind: 'fill', template: { id: 'doc_form' }, material: { id: 'doc_info' } })
    expect(await resolveFormEntry(api, input('再把这张报销单填一下', { remembered })))
      .toEqual({ kind: 'choose', formName: '报销单', candidates: [] })
  })
  it('keeps the material told before when the answer to 「要填的是哪一份？」 names the template (C2)', async () => {
    const lib = [...LIB, docx('doc_form2', '入职登记表（2026版）.docx')]
    const { api } = fakeApi(lib, { ...FIELDS, doc_form2: ['姓名'] })
    const earlier = ['把这张入职登记表填一下，资料在《个人信息.docx》里']
    expect(await resolveFormEntry(api, input('《入职登记表（2026版）.docx》', { asks: false, earlier })))
      .toMatchObject({ kind: 'fill', template: { id: 'doc_form2' }, material: { id: 'doc_info' } })
  })
  it('never offers a document a fill made as a template (0.7.125)', async () => {
    const lib = [...LIB, docx('doc_made', '入职登记表（填写）'), docx('doc_made2', '入职登记表（填写）.docx')]
    const { api } = fakeApi(lib, { ...FIELDS, doc_made: ['紧急联系人'], doc_made2: ['签名'] })
    expect(await resolveFormEntry(api, input('把这张入职登记表填一下，资料在《个人信息.docx》里')))
      .toMatchObject({ kind: 'fill', template: { id: 'doc_form' }, material: { id: 'doc_info' } })
  })
  it('says a named Word document without {{}} cannot be filled when no form is named otherwise', async () => {
    const { api } = fakeApi(LIB, FIELDS)
    expect(await resolveFormEntry(api, input('《会议纪要.docx》这张表能帮我填吗'))).toEqual({ kind: 'noFields', title: '会议纪要.docx' })
  })
  it('says what it says when a named document is not in the library', async () => {
    const { api } = fakeApi(LIB, FIELDS)
    expect(await resolveFormEntry(api, input('把这张入职登记表填一下，资料在《不存在.docx》里')))
      .toEqual({ kind: 'say', text: '没有找到标题含《不存在.docx》的文档。' })
  })
  it('asks, never fills, when no form is named and nothing is named', async () => {
    const { api } = fakeApi(LIB, FIELDS)
    expect(await resolveFormEntry(api, input('这张表能帮我填吗'))).toEqual({ kind: 'choose', formName: null, candidates: ['入职登记表.docx'] })
  })
  it('leaves the turn to the model when the library cannot be read', async () => {
    const { api } = fakeApi(LIB, FIELDS, false)
    expect(await resolveFormEntry(api, input('把这张入职登记表填一下，资料在《个人信息.docx》里'))).toEqual({ kind: 'none' })
  })
})
