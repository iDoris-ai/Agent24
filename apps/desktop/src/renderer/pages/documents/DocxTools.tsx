// E1b/D5a: a Word document's own tools on its page. 「填写模板」 fills its
// {{fields}} and 「改写段落」 rewrites paragraphs (plainly or as tracked
// changes); both make a NEW document through the documents module (the same
// `fill` / `patch` the CLI and the Agent's tools use), then it can be saved to
// Downloads.

import { useState } from 'react'
import { useT } from '../../i18n'
import { docxToolsMessages } from './DocxTools.i18n'
import { makeDocx } from './compose'
import { describeError } from './importStore'

type Api = Window['agent24']['documents']
type Para = { index: number; text: string; in_table: boolean }
type Made = { id: string | null; title: string }

/** 「<名>（suffix）」 as the module names it, for the note. */
export function madeTitle(title: string, suffix: string): string {
  return `${title.replace(/\.docx$/i, '')}${suffix}`
}

/** Only the fields given a value (trimmed of nothing: a value is the user's text). */
export function filledValues(values: Record<string, string>): Record<string, string> {
  return Object.fromEntries(Object.entries(values).filter(([, v]) => v !== ''))
}

export default function DocxTools({ api, documentId, title, onAdded }: {
  api: Api
  documentId: string
  title: string
  onAdded?: () => void
}) {
  const t = useT(docxToolsMessages)
  const [mode, setMode] = useState<'none' | 'fill' | 'patch'>('none')
  const [fields, setFields] = useState<string[] | null>(null)
  const [values, setValues] = useState<Record<string, string>>({})
  const [paras, setParas] = useState<Para[] | null>(null)
  const [pick, setPick] = useState(0)
  const [text, setText] = useState('')
  const [tracked, setTracked] = useState(true)
  const [busy, setBusy] = useState(false)
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null)
  const [made, setMade] = useState<Made | null>(null)
  const [saved, setSaved] = useState<string | null>(null)

  const reset = () => { setNote(null); setMade(null); setSaved(null) }

  const openFill = async () => {
    reset()
    setMode('fill')
    const res = await api.request({ op: 'fields', documentId })
    if (!res.ok) { setNote({ ok: false, text: t('failed', { error: describeError(res) }) }); return }
    setFields(res.data.fields.map((f) => f.name))
    setValues({})
  }

  const openPatch = async () => {
    reset()
    setMode('patch')
    const res = await api.request({ op: 'paragraphs', documentId })
    if (!res.ok) { setNote({ ok: false, text: t('failed', { error: describeError(res) }) }); return }
    const list = res.data.paragraphs.filter((p) => p.text.trim() !== '')
    setParas(list)
    setPick(list[0]?.index ?? 0)
    setText(list[0]?.text ?? '')
  }

  const run = async (req: Parameters<typeof makeDocx>[1], suffix: string) => {
    reset()
    setBusy(true)
    try {
      const out = await makeDocx(api, req)
      if (!out.ok) { setNote({ ok: false, text: t('failed', { error: out.message }) }); return }
      const name = madeTitle(title, suffix)
      setMade({ id: out.documentId, title: name })
      setNote({ ok: true, text: t('done', { title: name }) })
      onAdded?.()
    } catch (e) {
      setNote({ ok: false, text: t('failed', { error: e instanceof Error ? e.message : String(e) }) })
    } finally {
      setBusy(false)
    }
  }

  const fill = () => {
    const given = filledValues(values)
    if (Object.keys(given).length === 0) { setNote({ ok: false, text: t('fill.empty') }); return }
    void run({ op: 'fill', documentId, values: given }, '（填写）')
  }

  const patch = () => {
    const before = paras?.find((p) => p.index === pick)
    if (!before || before.text === text) { setNote({ ok: false, text: t('patch.same') }); return }
    void run({ op: 'patch', documentId, edits: [{ index: pick, text, tracked, expect: before.text }] }, '（修订）')
  }

  const save = async () => {
    if (!made?.id) return
    const out = await api.saveDownload(made.id, `${made.title}.docx`)
    if (out.ok && out.docxName) setSaved(out.docxName)
    else setNote({ ok: false, text: t('saveFailed', { error: out.message ?? out.code ?? '' }) })
  }

  return (
    <div style={{ margin: '8px 0' }}>
      <div style={{ display: 'flex', gap: 8 }}>
        <button className="btn btn-ghost" disabled={busy} onClick={() => void (mode === 'fill' ? setMode('none') : openFill())}>
          {mode === 'fill' ? t('close') : t('fill.load')}
        </button>
        <button className="btn btn-ghost" disabled={busy} onClick={() => void (mode === 'patch' ? setMode('none') : openPatch())}>
          {mode === 'patch' ? t('close') : t('patch.load')}
        </button>
      </div>
      {mode === 'fill' && fields !== null && (
        <section aria-label={t('fill.title')} style={{ marginTop: 8 }}>
          {fields.length === 0 ? <p>{t('fill.none')}</p> : (
            <>
              <p style={{ color: 'var(--muted)', fontSize: 12 }}>{t('fill.hint')}</p>
              {fields.map((name) => (
                <label key={name} style={{ display: 'flex', gap: 8, alignItems: 'center', margin: '4px 0' }}>
                  <span style={{ minWidth: 96 }}>{name}</span>
                  <input
                    aria-label={name}
                    value={values[name] ?? ''}
                    onChange={(e) => setValues((prev) => ({ ...prev, [name]: e.target.value }))}
                    style={{ flex: 1 }}
                  />
                </label>
              ))}
              <button className="btn btn-primary" disabled={busy} onClick={fill}>{busy ? t('busy') : t('fill.make')}</button>
            </>
          )}
        </section>
      )}
      {mode === 'patch' && paras !== null && (
        <section aria-label={t('patch.title')} style={{ marginTop: 8 }}>
          <label style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            <span>{t('patch.pick')}</span>
            <select
              aria-label={t('patch.pick')}
              value={pick}
              onChange={(e) => {
                const index = Number(e.target.value)
                setPick(index)
                setText(paras.find((p) => p.index === index)?.text ?? '')
              }}
              style={{ flex: 1, maxWidth: 480 }}
            >
              {paras.map((p) => (
                <option key={p.index} value={p.index}>{`${p.index + 1}. ${p.text.slice(0, 40)}${p.text.length > 40 ? '…' : ''}`}</option>
              ))}
            </select>
          </label>
          <textarea aria-label={t('patch.title')} value={text} onChange={(e) => setText(e.target.value)} rows={4} style={{ width: '100%', marginTop: 4 }} />
          <label style={{ display: 'block', margin: '4px 0' }}>
            <input type="checkbox" checked={tracked} onChange={(e) => setTracked(e.target.checked)} /> {t('patch.tracked')}
          </label>
          <button className="btn btn-primary" disabled={busy} onClick={patch}>{busy ? t('busy') : t('patch.make')}</button>
        </section>
      )}
      {note && <p role={note.ok ? 'status' : 'alert'}>{note.text}</p>}
      {made?.id && (
        <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
          <button className="btn btn-ghost" onClick={() => void save()}>{t('save')}</button>
          {saved && (
            <>
              <span role="status">{t('saved', { name: saved })}</span>
              <button className="btn btn-ghost" onClick={() => void api.revealDownload(saved)}>{t('reveal')}</button>
            </>
          )}
        </div>
      )}
    </div>
  )
}
