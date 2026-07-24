import { describe, it, expect } from 'vitest'
import { previewNextFire, formatPreview } from './cronPreview'

const NOW = new Date('2026-07-24T10:00:00Z')

describe('previewNextFire', () => {
  it('every: now + secs, bounds enforced', () => {
    expect(formatPreview(previewNextFire({ type: 'every', secs: 3600 }, NOW).next!)).toBe(
      '2026-07-24 11:00:00 UTC',
    )
    expect(previewNextFire({ type: 'every', secs: 30 }, NOW).error).toBeTruthy()
    expect(previewNextFire({ type: 'every', secs: 100_000 }, NOW).error).toBeTruthy()
  })

  it('at: future timestamp accepted, past rejected', () => {
    const r = previewNextFire({ type: 'at', ts: '2026-07-24T12:00:00Z' }, NOW)
    expect(formatPreview(r.next!)).toBe('2026-07-24 12:00:00 UTC')
    expect(previewNextFire({ type: 'at', ts: '2026-07-24T09:00:00Z' }, NOW).error).toBe('该时间已过')
    expect(previewNextFire({ type: 'at', ts: 'not-a-date' }, NOW).error).toBeTruthy()
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

  it('cron: invalid expressions error', () => {
    expect(previewNextFire({ type: 'cron', expr: 'nonsense' }, NOW).error).toBeTruthy()
    expect(previewNextFire({ type: 'cron', expr: '' }, NOW).error).toBeTruthy()
    expect(previewNextFire({ type: 'cron', expr: '99 * * * *' }, NOW).error).toBeTruthy()
    // wrong field count
    expect(previewNextFire({ type: 'cron', expr: '0 8 * *' }, NOW).error).toBeTruthy()
  })

  it('unknown type errors', () => {
    // @ts-expect-error deliberately wrong
    expect(previewNextFire({ type: 'bogus' }, NOW).error).toBeTruthy()
  })
})
