import { useEffect, useRef, useState } from 'react'
import { useT } from '../../i18n'
import { listRuns } from '../agent/api'
import RunsPage from '../Runs'
import { settingsMessages } from '../Settings.i18n'
import { summarizeActivity, type ActivityNumbers } from './activityStats'

const PLACEHOLDER = '\u2014'
const REFRESH_MS = 3000

function showCount(n: number | null): string {
  return n === null ? PLACEHOLDER : String(n)
}

/** Runs, plus four figures from the same `listRuns` feed. A figure that
 *  cannot be computed renders an em dash. */
export function ActivityTab() {
  const t = useT(settingsMessages)
  const [stats, setStats] = useState<ActivityNumbers | null>(null)
  const seq = useRef(0)

  useEffect(() => {
    let alive = true
    const load = () => {
      const ticket = ++seq.current
      listRuns()
        .then((runs) => {
          if (alive && ticket === seq.current) setStats(summarizeActivity(runs))
        })
        .catch(() => {
          if (alive && ticket === seq.current) setStats(summarizeActivity(null))
        })
    }
    load()
    const timer = setInterval(load, REFRESH_MS)
    return () => { alive = false; clearInterval(timer) }
  }, [])

  const view = stats ?? summarizeActivity(null)
  const cards: { label: 'activity.running' | 'activity.waiting' | 'activity.failedToday' | 'activity.tokensToday'; value: number | null }[] = [
    { label: 'activity.running', value: view.running },
    { label: 'activity.waiting', value: view.waiting },
    { label: 'activity.failedToday', value: view.failedToday },
    { label: 'activity.tokensToday', value: view.tokensToday },
  ]

  return (
    <div>
      <dl className="settings-stat-grid" role="group" aria-label={t('activity.summary')}>
        {cards.map((card) => (
          <div key={card.label} className="settings-stat">
            <dt>{t(card.label)}</dt>
            <dd>{showCount(card.value)}</dd>
          </div>
        ))}
      </dl>
      <div className="settings-activity">
        <RunsPage />
      </div>
    </div>
  )
}
