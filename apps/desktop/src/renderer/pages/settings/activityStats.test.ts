import { describe, expect, it } from 'vitest'
import { summarizeActivity, type ActivityRun } from './activityStats'

const now = new Date(2026, 9, 28, 15, 0, 0)

function at(day: number, hour: number): string {
  return new Date(2026, 9, day, hour, 0, 0).toISOString()
}

function run(partial: Partial<ActivityRun> & Pick<ActivityRun, 'status'>): ActivityRun {
  return { created_at: at(28, 9), usage: { total_tokens: 1 }, ...partial }
}

describe('summarizeActivity', () => {
  it('returns blanks when the run list never arrived', () => {
    expect(summarizeActivity(null, now)).toEqual({
      running: null, waiting: null, failedToday: null, tokensToday: null,
    })
  })

  it('returns zeros for an empty list', () => {
    expect(summarizeActivity([], now)).toEqual({
      running: 0, waiting: 0, failedToday: 0, tokensToday: 0,
    })
  })

  it('counts running, waiting, failures that ended today, and tokens created today', () => {
    const runs: ActivityRun[] = [
      run({ status: 'running', created_at: at(28, 9), usage: { total_tokens: 10 } }),
      run({ status: 'running', created_at: at(27, 9), usage: { total_tokens: 5 } }),
      run({ status: 'queued', created_at: at(28, 8), usage: { total_tokens: 4 } }),
      run({ status: 'awaiting_approval', created_at: at(28, 10), usage: { total_tokens: 1 } }),
      run({ status: 'failed', created_at: at(27, 18), ended_at: at(28, 1), usage: { total_tokens: 7 } }),
      run({ status: 'failed', created_at: at(27, 12), ended_at: at(27, 13), usage: { total_tokens: 3 } }),
      run({ status: 'completed', created_at: at(28, 11), usage: { total_tokens: 100 } }),
    ]
    expect(summarizeActivity(runs, now)).toEqual({
      running: 2,
      waiting: 1,
      failedToday: 1,
      tokensToday: 115,
    })
  })

  it('uses creation time when a failure has no usable end time', () => {
    const runs = [
      run({ status: 'failed', created_at: at(28, 9), ended_at: 'not-a-date', usage: { total_tokens: 2 } }),
      run({ status: 'failed', created_at: at(27, 9), ended_at: '', usage: { total_tokens: 2 } }),
    ]
    expect(summarizeActivity(runs, now).failedToday).toBe(1)
  })

  it('blanks tokens today when a run created today has no finite total', () => {
    expect(summarizeActivity([
      run({ status: 'completed', usage: {} }),
    ], now).tokensToday).toBeNull()
    expect(summarizeActivity([
      run({ status: 'completed', usage: { total_tokens: Number.NaN } }),
    ], now).tokensToday).toBeNull()
    expect(summarizeActivity([
      run({ status: 'completed', usage: null }),
    ], now).tokensToday).toBeNull()
    // A gap on an older run does not erase today's known total.
    expect(summarizeActivity([
      run({ status: 'completed', created_at: at(27, 9), usage: null }),
      run({ status: 'completed', created_at: at(28, 9), usage: { total_tokens: 8 } }),
    ], now).tokensToday).toBe(8)
  })
})
