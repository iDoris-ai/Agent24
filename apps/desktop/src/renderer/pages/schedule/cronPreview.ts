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
  /** Human-readable text for display (error or info); null when a time shows */
  message: string | null
  /** true = the spec is INVALID (blocks create, shown red). A valid spec
   *  whose next fire is simply beyond the preview window is NOT an error. */
  isError: boolean
  /** true when the preview may drift from the server's tz-accurate result */
  approximate: boolean
}

const MIN_EVERY_SECS = 60
const MAX_EVERY_SECS = 86_400
// Cap the minute-by-minute cron search (~366 days) so a never-matching
// expression can't spin forever. This bounds the PREVIEW only — it never
// gates schedule creation (a valid cron firing >1y out, e.g. Feb 29, is still
// creatable; the server computes its real next_run_at).
const MAX_SEARCH_MINUTES = 366 * 24 * 60

/** Compile one cron field into a set of allowed integers in [min, max], or
 *  null when the field is malformed (strict: no empty parts, one `/`, one
 *  `-`). */
function parseField(field: string, min: number, max: number): Set<number> | null {
  if (field === '') return null
  const allowed = new Set<number>()
  for (const part of field.split(',')) {
    if (part === '') return null // e.g. "1,,2"
    const slash = part.split('/')
    if (slash.length > 2) return null // e.g. "*/2/3"
    const [base, stepStr] = slash
    const step = stepStr === undefined ? 1 : Number(stepStr)
    if (!Number.isInteger(step) || step < 1) return null

    let lo: number
    let hi: number
    if (base === '*') {
      lo = min
      hi = max
    } else if (base.includes('-')) {
      const range = base.split('-')
      if (range.length !== 2) return null // e.g. "1-2-3"
      const [a, b] = range.map(Number)
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
  // cron dow: 0-6 (Sun=0). Accept 7 as an alias for Sunday by parsing the
  // field over 0..7 (so "1-7" / "*/7" stay valid) then folding 7 into 0 —
  // NOT a naive string replace, which would corrupt "*/7", "17", etc.
  const dowRaw = parseField(f[4], 0, 7)
  if (!min || !hour || !dom || !month || !dowRaw) return null
  const dow = new Set<number>()
  for (const v of dowRaw) dow.add(v === 7 ? 0 : v)
  return {
    min,
    hour,
    dom,
    month,
    dow,
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
          message: `每隔秒数须为 ${MIN_EVERY_SECS}–${MAX_EVERY_SECS}`,
          isError: true,
          approximate: false,
        }
      }
      return {
        next: new Date(now.getTime() + secs * 1000),
        message: null,
        isError: false,
        approximate: false,
      }
    }
    case 'at': {
      const ts = spec.ts ?? ''
      const at = new Date(ts)
      if (Number.isNaN(at.getTime())) {
        return { next: null, message: '时间格式无效（需 ISO 8601）', isError: true, approximate: false }
      }
      if (at.getTime() <= now.getTime()) {
        return { next: null, message: '该时间已过', isError: true, approximate: false }
      }
      return { next: at, message: null, isError: false, approximate: false }
    }
    case 'cron': {
      const expr = (spec.expr ?? '').trim()
      if (!expr) return { next: null, message: '请输入 cron 表达式', isError: true, approximate: false }
      // Syntax validity gates creation; the search window only gates display.
      if (!parseCron(expr)) {
        return { next: null, message: 'cron 表达式无效', isError: true, approximate: false }
      }
      const tz = spec.tz?.trim()
      const approximate = !!tz && tz !== 'UTC' && tz !== 'Etc/UTC'
      const next = nextCronFire(expr, now)
      if (!next) {
        // Valid syntax, just no fire within the preview window (e.g. Feb 29).
        // NOT an error — the server computes the real next_run_at on save.
        return {
          next: null,
          message: '一年内不触发（服务端将计算下次时间）',
          isError: false,
          approximate,
        }
      }
      return { next, message: null, isError: false, approximate }
    }
    default:
      return { next: null, message: '未知调度类型', isError: true, approximate: false }
  }
}

/** Format a preview instant for display (UTC, second precision). */
export function formatPreview(d: Date): string {
  return `${d.toISOString().slice(0, 19).replace('T', ' ')} UTC`
}
