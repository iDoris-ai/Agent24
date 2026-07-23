import { describe, it, expect } from 'vitest'
import { IpcChannels } from './ipc-types'

describe('IpcChannels', () => {
  it('declares app channels with namespaced names', () => {
    expect(IpcChannels.AppPing).toBe('app:ping')
    expect(IpcChannels.AppVersion).toBe('app:version')
  })

  it('all channel names are colon-namespaced', () => {
    for (const channel of Object.values(IpcChannels)) {
      expect(channel).toMatch(/^[a-z][a-z0-9-]*:[a-z][a-z0-9:_-]*$/)
    }
  })
})
