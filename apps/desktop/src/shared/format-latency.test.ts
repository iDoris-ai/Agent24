import { describe, it, expect } from 'vitest'
import { formatMs } from './format-latency'

describe('formatMs', () => {
  it('shows plain ms below 1000', () => {
    expect(formatMs(0)).toBe('0 ms')
    expect(formatMs(412)).toBe('412 ms')
    expect(formatMs(999)).toBe('999 ms')
  })

  it('adds a thousands separator at/above 1000, without rounding to seconds', () => {
    expect(formatMs(1000)).toBe('1,000 ms')
    expect(formatMs(1834)).toBe('1,834 ms')
    expect(formatMs(12345)).toBe('12,345 ms')
  })

  it('rounds to the nearest whole millisecond', () => {
    expect(formatMs(412.6)).toBe('413 ms')
    expect(formatMs(1834.4)).toBe('1,834 ms')
  })

  it('clamps a negative or non-finite value to 0 ms rather than showing garbage', () => {
    expect(formatMs(-5)).toBe('0 ms')
    expect(formatMs(NaN)).toBe('0 ms')
  })
})
