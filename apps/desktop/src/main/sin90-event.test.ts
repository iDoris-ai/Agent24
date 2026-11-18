import { describe, expect, it } from 'vitest'
import type { ModuleEvent } from './agentear-events'
import { sin90Event } from './sin90-event'

function frame(module: string, kind: string, payload: Record<string, unknown> = {}): ModuleEvent {
  return { module, kind, payload }
}

describe('sin90Event', () => {
  it('forwards a Sin90 frame and drops every other module', () => {
    expect(sin90Event(frame('sin90', 'task.transitioned', { task_id: 'a', to: 'done' }))).toEqual({
      kind: 'task.transitioned', payload: { task_id: 'a', to: 'done' },
    })
    expect(sin90Event(frame('documents', 'document.imported', { document_id: 'd' }))).toBeNull()
  })
})
