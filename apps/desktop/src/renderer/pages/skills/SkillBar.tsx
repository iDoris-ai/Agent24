import { useCallback, useEffect, useId, useRef, useState } from 'react'
import { useT } from '../../i18n'
import { notifySkillsChanged } from '../chat/session-skill'
import { skillBarMessages } from './SkillBar.i18n'
import {
  parseComponents,
  parseSkillDetail,
  parseSkillList,
  skillBarRows,
  type ActivationStep,
  type SkillBarRow,
} from './model'

type Proxy = Window['agent24']['backendProxy']

/** Throws when the list itself cannot be read: that is not "no skills". */
async function loadRows(proxy: Proxy): Promise<SkillBarRow[]> {
  const listRes = await proxy({ method: 'GET', path: '/api/v1/skills' })
  if (!listRes.ok) throw new Error('skill list unavailable')
  const list = parseSkillList(listRes.data)
  const details = new Map<string, NonNullable<ReturnType<typeof parseSkillDetail>>>()
  for (const skillId of [...new Set(list.map((item) => item.skill_id))]) {
    const res = await proxy({ method: 'GET', path: `/api/v1/skills/${encodeURIComponent(skillId)}` })
    if (!res.ok) continue
    const detail = parseSkillDetail(res.data)
    if (detail) details.set(skillId, detail)
  }
  const componentsRes = await proxy({ method: 'GET', path: '/api/v1/components' })
  const components = componentsRes.ok ? parseComponents(componentsRes.data) : []
  return skillBarRows(list, details, components)
}

/** Skills on the capabilities page. Activate and roll back go through main. */
export default function SkillBar() {
  const t = useT(skillBarMessages)
  const headingId = useId()
  const [rows, setRows] = useState<SkillBarRow[] | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [loadFailed, setLoadFailed] = useState(false)
  const [result, setResult] = useState<{ skillId: string; text: string; ok: boolean } | null>(null)

  // Rows older than the daemon's pointer would submit a stale pair: while the
  // last refresh failed, every action stays off.
  const [stale, setStale] = useState(false)
  const staleRef = useRef(false)

  const reload = useCallback((): Promise<boolean> => {
    const proxy = window.agent24?.backendProxy
    if (!proxy) {
      setRows([])
      return Promise.resolve(true)
    }
    return loadRows(proxy).then(
      (next) => { staleRef.current = false; setStale(false); setLoadFailed(false); setRows(next); return true },
      () => { staleRef.current = true; setStale(true); setLoadFailed(true); setRows((prev) => prev ?? []); return false },
    )
  }, [])

  useEffect(() => {
    void reload()
    const again = () => { if (staleRef.current) void reload() }
    window.addEventListener('focus', again)
    return () => window.removeEventListener('focus', again)
  }, [reload])

  const originLabel = (origin: string) => origin === 'user' ? t('origin.user') : origin === 'imported' ? t('origin.imported') : origin

  const describe = (skillId: string, res: { ok: boolean; status: number; data?: unknown }, done: 'result.activated' | 'result.rolledBack') => {
    const data = res.data as { result?: unknown; unchanged?: unknown; error?: { code?: unknown } } | null
    if (res.ok) {
      const unchanged = data?.unchanged === true || data?.result === 'unchanged'
      return { skillId, ok: true, text: t(unchanged ? 'result.unchanged' : done) }
    }
    const code = typeof data?.error?.code === 'string' ? data.error.code : res.status === 499 ? 'cancelled' : `http_${res.status}`
    const key = `err.${code}` as Parameters<typeof t>[0]
    const known = [
      'err.skill_activation_mixed', 'err.skill_pointer_conflict', 'err.skill_eval_empty', 'err.skill_origin_mismatch',
      'err.skill_revision_not_found', 'err.skill_activation_not_found', 'err.cancelled',
    ].includes(key)
    if (code === 'cancelled') return { skillId, ok: false, text: t('err.cancelled') }
    return { skillId, ok: false, text: t('result.failed', { code, message: known ? t(key) : t('err.generic') }) }
  }

  const run = (row: SkillBarRow, call: Promise<{ ok: boolean; status: number; data?: unknown }>, done: 'result.activated' | 'result.rolledBack') => {
    setBusy(row.skillId + row.origin)
    setResult(null)
    void call.then(
      (res) => setResult(describe(row.skillId, res, done)),
      () => setResult(describe(row.skillId, { ok: false, status: 0 }, done)),
    ).then(() => reload()).finally(() => { setBusy(null); notifySkillsChanged() })
  }

  const activate = (row: SkillBarRow, step: ActivationStep) => {
    if (busy) return
    const call = window.agent24?.skillsActivate
    if (!call) return
    run(row, call({ skillId: row.skillId, bodyRevisionId: step.bodyRevisionId, evalsRevisionId: step.evalsRevisionId }), 'result.activated')
  }

  const rollback = (row: SkillBarRow) => {
    if (!row.rollbackActivationId || busy) return
    const call = window.agent24?.skillsRollback
    if (!call) return
    run(row, call({ skillId: row.skillId, rollbackActivationId: row.rollbackActivationId }), 'result.rolledBack')
  }

  return (
    <section className="cap-group skill-bar" aria-labelledby={headingId}>
      <h2 id={headingId}>{t('section.title')}</h2>
      {loadFailed && <p role="alert" className="skill-dep-bad">{t('section.loadFailed')}</p>}
      {result && <p role="status" className={result.ok ? 'cap-blurb' : 'skill-dep-bad'}>{result.skillId}: {result.text}</p>}
      {rows === null || (loadFailed && rows.length === 0) ? null : rows.length === 0 ? <p className="cap-blurb">{t('section.empty')}</p> : (
        <table className="skill-table">
          <thead>
            <tr>
              <th>{t('col.name')}</th>
              <th>{t('col.version')}</th>
              <th>{t('col.origin')}</th>
              <th title={t('col.depsHint')}>{t('col.deps')}</th>
              <th>{t('col.mode')}</th>
              <th>{t('col.consistency')}</th>
              <th>{t('col.evals')}</th>
              <th>{t('col.digest')}</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => {
              const bad = row.requiresUnreadable || row.requires.some((dep) => dep.issue !== null)
              const key = `${row.skillId}:${row.origin}`
              return (
                <tr key={key} className={bad ? 'skill-row-bad' : undefined} aria-label={`${row.skillId} ${originLabel(row.origin)}`}>
                  <td>{row.skillId}</td>
                  <td>{row.semver}</td>
                  <td>{originLabel(row.origin)}</td>
                  <td>
                    {row.requiresUnreadable ? <span className="skill-dep-bad">{t('deps.unreadable')}</span>
                      : row.requires.length === 0 ? t('deps.none') : row.requires.map((dep, index) => (
                        <span key={dep.id}>
                          {index > 0 ? ', ' : null}
                          <span className={dep.issue ? 'skill-dep-bad' : undefined}>{dep.id}</span>
                          {dep.issue && <span className="skill-dep-bad"> ({t(dep.issue === 'missing' ? 'dep.missing' : 'dep.ambiguous')})</span>}
                        </span>
                      ))}
                  </td>
                  <td>{t('mode.manual')}</td>
                  <td>{row.consistency ? `${row.consistency.consistent}/${row.consistency.total}` : '—'}</td>
                  <td>{t(row.evals === 'changed' ? 'evals.changed' : row.evals === 'same' ? 'evals.same' : 'evals.none')}</td>
                  <td>{row.digest8}</td>
                  <td className="skill-actions">
                    {row.activations.length === 0 ? (
                      <button type="button" className="btn btn-primary" disabled>{t('action.activate')}</button>
                    ) : row.activations.map((step) => (
                      <button
                        key={step.side}
                        type="button"
                        className="btn btn-primary"
                        disabled={busy !== null || stale}
                        onClick={() => activate(row, step)}
                      >
                        {t(step.side === 'body' ? 'action.activateBody' : step.side === 'evals' ? 'action.activateEvals' : 'action.activate')}
                      </button>
                    ))}
                    {row.rollbackActivationId && (
                      <button type="button" className="btn btn-ghost" disabled={busy !== null || stale} onClick={() => rollback(row)}>
                        {t('action.rollback')}
                      </button>
                    )}
                  </td>
                </tr>
              )
            })}
          </tbody>
        </table>
      )}
    </section>
  )
}
