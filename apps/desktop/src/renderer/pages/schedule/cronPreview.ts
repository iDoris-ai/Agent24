// Client-side next-fire preview for the Schedules form. The daemon is
// authoritative (it recomputes next_run_at on save with full timezone/DST
// handling); this is a live UI preview only. Cron is computed in UTC — when a
// non-UTC timezone is selected the preview is flagged `approximate` because
// the browser cannot reproduce the server's tz/DST math exactly.

export interface ScheduleSpecInput {
  type: 'cron' | 'every' | 'at'
  expr?: string
  tz?: string
  secs?: number
  ts?: string
}

export interface PreviewResult {
  next: Date | null
  error: string | null
  /** true when the preview may drift from the server's tz-accurate result */
  approximate: boolean
}

const MIN_EVERY_SECS = 60
const MAX_EVERY_SECS = 86_400
// Cap the minute-by-minute cron search (~366 days) so a never-matching
// expression can't spin forever.
const MAX_SEARCH_MINUTES = 366 * 24 * 60

/** Compile one cron field into a set of allowed integers in [min, max]. */
function parseField(field: string, min: number, max: number): Set<number> | null {
  const allowed = new Set<number>()
  for (const part of field.split(',')) {
    // step: base/step
    const [base, stepStr] = part.split('/')
    const step = stepStr === undefined ? 1 : Number(stepStr)
    if (!Number.isInteger(step) || step < 1) return null

    let lo: number
    let hi: number
    if (base === '*') {
      lo = min
      hi = max
    } else if (base.includes('-')) {
      const [a, b] = base.split('-').map(Number)
      if (!Number.isInteger(a) || !Number.isInteger(b)) return null
      lo = a
      hi = b
    } else {
      const v = Number(base)
      if (!Number.isInteger(v)) return null
      lo = v
      hi = stepStr === undefined ? v : max // "N/step" means from N to max
    }
    if (lo < min || hi > max || lo > hi) return null
    for (let v = lo; v <= hi; v += step) allowed.add(v)
  }
  return allowed.size > 0 ? allowed : null
}

/** Normalize a 5- or 6-field cron into (min, hour, dom, month, dow) matchers. */
function parseCron(expr: string): {
  min: Set<number>
  hour: Set<number>
  dom: Set<number>
  month: Set<number>
  dow: Set<number>
  domRestricted: boolean
  dowRestricted: boolean
} | null {
  const fields = expr.trim().split(/\s+/)
  // Accept 5-field POSIX or 6-field (leading seconds, which we ignore for a
  // minute-granularity preview).
  let f = fields
  if (fields.length === 6) f = fields.slice(1)
  else if (fields.length !== 5) return null

  const min = parseField(f[0], 0, 59)
  const hour = parseField(f[1], 0, 23)
  const dom = parseField(f[2], 1, 31)
  const month = parseField(f[3], 1, 12)
  // cron dow: 0-6 (Sun=0); also accept 7 as Sunday
  const dowRaw = parseField(f[4].replace(/7/g, '0'), 0, 6)
  if (!min || !hour || !dom || !month || !dowRaw) return null
  return {
    min,
    hour,
    dom,
    month,
    dow: dowRaw,
    domRestricted: f[2] !== '*',
    dowRestricted: f[4] !== '*',
  }
}

function nextCronFire(expr: string, from: Date): Date | null {
  const c = parseCron(expr)
  if (!c) return null
  // Start at the next whole minute (UTC), seconds/millis zeroed.
  const t = new Date(from.getTime())
  t.setUTCSeconds(0, 0)
  t.setUTCMinutes(t.getUTCMinutes() + 1)

  for (let i = 0; i < MAX_SEARCH_MINUTES; i++) {
    const month = t.getUTCMonth() + 1
    const dom = t.getUTCDate()
    const dow = t.getUTCDay()
    const hour = t.getUTCHours()
    const minute = t.getUTCMinutes()

    // Standard cron day semantics: if BOTH day-of-month and day-of-week are
    // restricted, a match on EITHER fires; otherwise both must match.
    const domOk = c.dom.has(dom)
    const dowOk = c.dow.has(dow)
    const dayOk =
      c.domRestricted && c.dowRestricted ? domOk || dowOk : domOk && dowOk

    if (c.month.has(month) && dayOk && c.hour.has(hour) && c.min.has(minute)) {
      return new Date(t.getTime())
    }
    t.setUTCMinutes(t.getUTCMinutes() + 1)
  }
  return null
}

/** Compute the next-fire preview for a schedule spec input. */
export function previewNextFire(spec: ScheduleSpecInput, now: Date): PreviewResult {
  switch (spec.type) {
    case 'every': {
      const secs = spec.secs ?? 0
      if (!Number.isInteger(secs) || secs < MIN_EVERY_SECS || secs > MAX_EVERY_SECS) {
        return {
          next: null,
          error: `每隔秒数须为 ${MIN_EVERY_SECS}–${MAX_EVERY_SECS}`,
          approximate: false,
        }
      }
      return { next: new Date(now.getTime() + secs * 1000), error: null, approximate: false }
    }
    case 'at': {
      const ts = spec.ts ?? ''
      const at = new Date(ts)
      if (Number.isNaN(at.getTime())) {
        return { next: null, error: '时间格式无效（需 ISO 8601）', approximate: false }
      }
      if (at.getTime() <= now.getTime()) {
        return { next: null, error: '该时间已过', approximate: false }
      }
      return { next: at, error: null, approximate: false }
    }
    case 'cron': {
      const expr = (spec.expr ?? '').trim()
      if (!expr) return { next: null, error: '请输入 cron 表达式', approximate: false }
      const next = nextCronFire(expr, now)
      if (!next) {
        return { next: null, error: 'cron 表达式无效或一年内不触发', approximate: false }
      }
      const tz = spec.tz?.trim()
      const approximate = !!tz && tz !== 'UTC' && tz !== 'Etc/UTC'
      return { next, error: null, approximate }
    }
    default:
      return { next: null, error: '未知调度类型', approximate: false }
  }
}

/** Format a preview instant for display (UTC, second precision). */
export function formatPreview(d: Date): string {
  return `${d.toISOString().slice(0, 19).replace('T', ' ')} UTC`
}
