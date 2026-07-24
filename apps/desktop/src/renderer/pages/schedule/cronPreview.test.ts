import { describe, it, expect } from 'vitest'
import { previewNextFire, formatPreview } from './cronPreview'

const NOW = new Date('2026-07-24T10:00:00Z')

describe('previewNextFire', () => {
  it('every: now + secs, bounds enforced', () => {
    expect(formatPreview(previewNextFire({ type: 'every', secs: 3600 }, NOW).next!)).toBe(
      '2026-07-24 11:00:00 UTC',
    )
    expect(previewNextFire({ type: 'every', secs: 30 }, NOW).message).toBeTruthy()
    expect(previewNextFire({ type: 'every', secs: 100_000 }, NOW).message).toBeTruthy()
  })

  it('at: future timestamp accepted, past rejected', () => {
    const r = previewNextFire({ type: 'at', ts: '2026-07-24T12:00:00Z' }, NOW)
    expect(formatPreview(r.next!)).toBe('2026-07-24 12:00:00 UTC')
    expect(previewNextFire({ type: 'at', ts: '2026-07-24T09:00:00Z' }, NOW).message).toBe('该时间已过')
    expect(previewNextFire({ type: 'at', ts: 'not-a-date' }, NOW).message).toBeTruthy()
  })

  it('cron: 5-field daily at 08:00 UTC', () => {
    const r = previewNextFire({ type: 'cron', expr: '0 8 * * *' }, NOW)
    // 10:00 is past 08:00 today → next is tomorrow 08:00
    expect(formatPreview(r.next!)).toBe('2026-07-25 08:00:00 UTC')
    expect(r.approximate).toBe(false)
  })

  it('cron: same-day when the time is still ahead', () => {
    const early = new Date('2026-07-24T07:00:00Z')
    const r = previewNextFire({ type: 'cron', expr: '0 8 * * *' }, early)
    expect(formatPreview(r.next!)).toBe('2026-07-24 08:00:00 UTC')
  })

  it('cron: step syntax */15 minutes', () => {
    const r = previewNextFire({ type: 'cron', expr: '*/15 * * * *' }, new Date('2026-07-24T10:07:00Z'))
    expect(formatPreview(r.next!)).toBe('2026-07-24 10:15:00 UTC')
  })

  it('cron: list + range fields', () => {
    // at minute 0, hours 9 or 17, Mon-Fri
    const r = previewNextFire({ type: 'cron', expr: '0 9,17 * * 1-5' }, NOW)
    // 2026-07-24 is a Friday; 10:00 is past 09:00 → next is 17:00 same day
    expect(formatPreview(r.next!)).toBe('2026-07-24 17:00:00 UTC')
  })

  it('cron: day-of-month OR day-of-week when both restricted', () => {
    // 1st of month OR every Monday, at 00:00
    const r = previewNextFire({ type: 'cron', expr: '0 0 1 * 1' }, new Date('2026-07-24T10:00:00Z'))
    // next Monday is 2026-07-27; 1st of Aug is 2026-08-01 — Monday comes first
    expect(formatPreview(r.next!)).toBe('2026-07-27 00:00:00 UTC')
  })

  it('cron: 6-field (leading seconds ignored for the minute preview)', () => {
    const r = previewNextFire({ type: 'cron', expr: '30 0 8 * * *' }, NOW)
    expect(formatPreview(r.next!)).toBe('2026-07-25 08:00:00 UTC')
  })

  it('cron: non-UTC timezone flags the preview approximate', () => {
    const r = previewNextFire({ type: 'cron', expr: '0 8 * * *', tz: 'Asia/Shanghai' }, NOW)
    expect(r.next).not.toBeNull()
    expect(r.approximate).toBe(true)
  })

  it('cron: invalid expressions are flagged isError', () => {
    for (const expr of ['nonsense', '', '99 * * * *', '0 8 * *', '1,,2 * * * *', '*/2/3 * * * *', '1-2-3 * * * *']) {
      const r = previewNextFire({ type: 'cron', expr }, NOW)
      expect(r.message, expr).toBeTruthy()
      expect(r.isError, expr).toBe(true)
    }
  })

  it('cron: dow 7 is Sunday without corrupting */7 or ranges', () => {
    // "*/7" in the minute field must NOT be mangled to "*/0"
    const everySeven = previewNextFire({ type: 'cron', expr: '*/7 * * * *' }, new Date('2026-07-24T10:00:00Z'))
    expect(everySeven.isError).toBe(false)
    expect(formatPreview(everySeven.next!)).toBe('2026-07-24 10:07:00 UTC')
    // dow 7 == 0 == Sunday: next Sunday 00:00 after Fri 2026-07-24 is 07-26
    const sunday = previewNextFire({ type: 'cron', expr: '0 0 * * 7' }, new Date('2026-07-24T10:00:00Z'))
    expect(formatPreview(sunday.next!)).toBe('2026-07-26 00:00:00 UTC')
    // "1-7" dow range stays valid
    expect(previewNextFire({ type: 'cron', expr: '0 0 * * 1-7' }, NOW).isError).toBe(false)
  })

  it('cron: valid syntax with no fire in a year is not an error (creatable)', () => {
    // Feb 29 only exists on leap years — no fire within 366 days of 2026-07
    const r = previewNextFire({ type: 'cron', expr: '0 0 29 2 *' }, NOW)
    expect(r.next).toBeNull()
    expect(r.isError).toBe(false) // must NOT block create
    expect(r.message).toContain('服务端')
  })

  it('unknown type errors', () => {
    // @ts-expect-error deliberately wrong
    expect(previewNextFire({ type: 'bogus' }, NOW).message).toBeTruthy()
  })
})
