import { describe, expect, it } from 'vitest'
import { createOwnership, MemoryEndpointHandoff } from './sidecar-contract'

describe('createOwnership', () => {
  it('only records a process group when the launcher proves one exists', () => {
    expect(createOwnership('creative', 123).processGroupId).toBeNull()
    expect(createOwnership('creative', 123, 123).processGroupId).toBe(123)
  })
})

describe('MemoryEndpointHandoff', () => {
  it('publishes a host-only endpoint and returns a defensive copy', () => {
    const handoff = new MemoryEndpointHandoff()
    const endpoint = { origin: 'http://127.0.0.1:4312', secret: 'test-secret' }

    handoff.publish('instance-a', endpoint)
    const snapshot = handoff.current()
    expect(snapshot).toEqual({ instanceId: 'instance-a', endpoint })
    expect(snapshot).not.toBe(endpoint)
    expect(snapshot?.endpoint).not.toBe(endpoint)
  })

  it('does not let an old child clear a newer endpoint', () => {
    const handoff = new MemoryEndpointHandoff()
    handoff.publish('instance-a', { origin: 'http://127.0.0.1:4312', secret: 'a' })
    handoff.publish('instance-b', { origin: 'http://127.0.0.1:4313', secret: 'b' })

    handoff.clear('instance-a')
    expect(handoff.current()?.instanceId).toBe('instance-b')

    handoff.clear('instance-b')
    expect(handoff.current()).toBeNull()
  })
})
