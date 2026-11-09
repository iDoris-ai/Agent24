import { describe, expect, it } from 'vitest'
import { promotionDue } from './chat-history'

// M4 A2: by default everything is an answer; a run becomes a task only on
// these two observations (the user's "hand it over" promotes directly, A4).
describe('promotionDue', () => {
  it('promotes a run that waits on an approval, whether or not the reply ended', () => {
    expect(promotionDue({ status: 'awaiting_approval', replyEnded: false })).toBe(true)
    expect(promotionDue({ status: 'awaiting_approval', replyEnded: true })).toBe(true)
  })

  it.each(['queued', 'running'])('promotes a run still %s once the page stopped waiting for its reply', status => {
    expect(promotionDue({ status, replyEnded: true })).toBe(true)
    expect(promotionDue({ status, replyEnded: false })).toBe(false)
  })

  it.each(['completed', 'failed', 'cancelled', 'unknown'])('never promotes a run that is %s', status => {
    expect(promotionDue({ status, replyEnded: true })).toBe(false)
    expect(promotionDue({ status, replyEnded: false })).toBe(false)
  })
})
