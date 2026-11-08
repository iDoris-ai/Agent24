import { describe, expect, it } from 'vitest'
import { BRIDGE_READ_CONCURRENCY, BRIDGE_READ_QUEUE_MS, BridgeReadBudget } from './knowledge-bridge-budget'

function harness() {
  let now = 0
  const timers: Array<{ at: number; fn: () => void; handle: object }> = []
  const budget = new BridgeReadBudget({
    now: () => now,
    setTimer: (fn, ms) => {
      const handle = {}
      timers.push({ at: now + ms, fn, handle })
      return handle
    },
    clearTimer: (handle) => {
      const i = timers.findIndex((t) => t.handle === handle)
      if (i >= 0) timers.splice(i, 1)
    },
  })
  const advance = (ms: number): void => {
    now += ms
    for (const t of [...timers].sort((a, b) => a.at - b.at)) {
      if (t.at <= now) {
        const i = timers.indexOf(t)
        if (i >= 0) timers.splice(i, 1)
        t.fn()
      }
    }
  }
  return { budget, advance, timers, now: () => now }
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0))

describe('BridgeReadBudget', () => {
  it('runs two at once, in FIFO order, and starts the next when a slot frees', async () => {
    const h = harness()
    const order: string[] = []
    let releaseA: (value: string) => void = () => {}
    const a = h.budget.run((signal) => {
      order.push('a')
      expect(signal.aborted).toBe(false)
      return new Promise<string>((resolve) => { releaseA = resolve })
    }, new AbortController().signal)
    const b = h.budget.run(() => { order.push('b'); return Promise.resolve('B') }, new AbortController().signal)
    const c = h.budget.run(() => { order.push('c'); return Promise.resolve('C') }, new AbortController().signal)
    expect(BRIDGE_READ_CONCURRENCY).toBe(2)
    expect(order).toEqual(['a', 'b'])
    await flush()
    expect(order).toEqual(['a', 'b', 'c'])
    expect(await b).toEqual({ kind: 'done', value: 'B' })
    expect(await c).toEqual({ kind: 'done', value: 'C' })
    releaseA('A')
    expect(await a).toEqual({ kind: 'done', value: 'A' })
  })

  it('a job queued for 10 s settles overloaded and never starts', async () => {
    const h = harness()
    const hang = () => new Promise<string>(() => {})
    void h.budget.run(hang, new AbortController().signal)
    void h.budget.run(hang, new AbortController().signal)
    let started = false
    const third = h.budget.run(() => { started = true; return Promise.resolve('no') }, new AbortController().signal)
    h.advance(BRIDGE_READ_QUEUE_MS - 1)
    await flush()
    let settled = false
    void third.then(() => { settled = true })
    await flush()
    expect(settled).toBe(false)
    expect(started).toBe(false)
    h.advance(1)
    expect(await third).toEqual({ kind: 'overloaded' })
    expect(started).toBe(false)
    expect(h.timers).toEqual([])
  })

  it('aborting while queued leaves the queue and does not take a slot', async () => {
    const h = harness()
    const calls: string[] = []
    const hang = (name: string) => () => { calls.push(name); return new Promise<string>(() => {}) }
    void h.budget.run(hang('a'), new AbortController().signal)
    void h.budget.run(hang('b'), new AbortController().signal)
    const queued = new AbortController()
    const third = h.budget.run(hang('c'), queued.signal)
    expect(calls).toEqual(['a', 'b'])
    queued.abort()
    expect(await third).toEqual({ kind: 'cancelled' })
    expect(calls).toEqual(['a', 'b'])
    void h.budget.run(hang('d'), new AbortController().signal)
    expect(calls).toEqual(['a', 'b'])
  })

  it('aborting while running aborts the task and settles cancelled', async () => {
    const h = harness()
    const caller = new AbortController()
    let saw = false
    const running = h.budget.run((signal) => new Promise<string>((resolve) => {
      signal.addEventListener('abort', () => { saw = true; resolve('late') })
    }), caller.signal)
    expect(saw).toBe(false)
    caller.abort()
    expect(saw).toBe(true)
    expect(await running).toEqual({ kind: 'cancelled' })
  })

  it('destroy cancels what is running and what is queued, and starts nothing more', async () => {
    const h = harness()
    let runningAborted = false
    const running = h.budget.run((signal) => new Promise<string>((resolve) => {
      signal.addEventListener('abort', () => { runningAborted = true; resolve('late') })
    }), new AbortController().signal)
    void h.budget.run(() => new Promise<string>(() => {}), new AbortController().signal)
    let queuedStarted = false
    const queued = h.budget.run(() => { queuedStarted = true; return Promise.resolve('no') }, new AbortController().signal)
    h.budget.destroy()
    expect(runningAborted).toBe(true)
    expect(queuedStarted).toBe(false)
    expect(await running).toEqual({ kind: 'cancelled' })
    expect(await queued).toEqual({ kind: 'cancelled' })
    let after = false
    expect(await h.budget.run(() => { after = true; return Promise.resolve('x') }, new AbortController().signal)).toEqual({ kind: 'cancelled' })
    expect(after).toBe(false)
    h.budget.destroy()
  })
})
