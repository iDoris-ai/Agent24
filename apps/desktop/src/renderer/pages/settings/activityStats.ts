// Figures for Settings › Activity, computed from the same run list the Runs
// page renders (`listRuns`). `null` means that figure cannot be known.
//
// Running counts status "running" only (queued stays queued). Waiting counts
// "awaiting_approval". Failed today is a failed run whose end — or whose
// creation, when it has not ended — falls on the local calendar day. Tokens
// today sum `usage.total_tokens` for runs created that day; if any of those
// runs has no finite total, the sum would undercount, so the card is blank.

export interface ActivityRun {
  status: string
  created_at?: string | null
  ended_at?: string | null
  usage?: { total_tokens?: number | null } | null
}

export interface ActivityNumbers {
  running: number | null
  waiting: number | null
  failedToday: number | null
  tokensToday: number | null
}

const UNKNOWN: ActivityNumbers = {
  running: null,
  waiting: null,
  failedToday: null,
  tokensToday: null,
}

function parsed(iso: string | null | undefined): Date | null {
  if (!iso) return null
  const d = new Date(iso)
  return Number.isNaN(d.getTime()) ? null : d
}

function localDay(d: Date): string {
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`
}

function onLocalDay(iso: string | null | undefined, today: string): boolean {
  const d = parsed(iso)
  return d !== null && localDay(d) === today
}

export function summarizeActivity(runs: readonly ActivityRun[] | null, now = new Date()): ActivityNumbers {
  if (runs == null) return UNKNOWN
  const today = localDay(now)
  let running = 0
  let waiting = 0
  let failedToday = 0
  let tokens = 0
  let tokensKnown = true
  for (const run of runs) {
    if (run.status === 'running') running += 1
    if (run.status === 'awaiting_approval') waiting += 1
    if (run.status === 'failed') {
      const when = parsed(run.ended_at) ?? parsed(run.created_at)
      if (when && localDay(when) === today) failedToday += 1
    }
    if (tokensKnown && onLocalDay(run.created_at, today)) {
      const n = run.usage?.total_tokens
      if (typeof n !== 'number' || !Number.isFinite(n)) tokensKnown = false
      else tokens += n
    }
  }
  return { running, waiting, failedToday, tokensToday: tokensKnown ? tokens : null }
}
