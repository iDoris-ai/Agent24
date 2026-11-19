import { describe, expect, it } from 'vitest'
import { filterHumanCommMessages } from './history-filter'

const item = (content: string, from = 'npub1a', to = 'npub1b') => ({ id: content, content, from, to, direction: 'in' as const, at: '' })

describe('filterHumanCommMessages', () => {
  it('filters Hyphae protocol messages and self messages after trimming', () => {
    expect(filterHumanCommMessages([
      item('  a24-liveness-canary ping'), item(' agent24.friend/v1 invite'),
      item('{"version":"f4/1"}'), item('{"protocol":"hyphae-behavior/1","type":"query"}'),
      item('self', 'npub1a', 'npub1a'), item('hello'),
    ])).toEqual([item('hello')])
  })
})
