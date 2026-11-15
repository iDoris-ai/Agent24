import { describe, it, expect } from 'vitest'
import { serviceDaemonOf } from './service-daemon'

describe('serviceDaemonOf (M5 B6 c2)', () => {
  const target = { socketPath: '/data/enroll.sock', generation: 'gen-1' }

  it('is the enrolled service daemon by value: its pid and generation', () => {
    expect(serviceDaemonOf({ presenceTarget: () => target, externalDaemonPid: () => 4242 })).toEqual({ pid: 4242, generation: 'gen-1' })
  })

  it('is null without an enrolment, even with an external pid (a reused legacy daemon)', () => {
    expect(serviceDaemonOf({ presenceTarget: () => null, externalDaemonPid: () => 4242 })).toBeNull()
  })

  it('is null without a pid', () => {
    expect(serviceDaemonOf({ presenceTarget: () => target, externalDaemonPid: () => null })).toBeNull()
  })
})
