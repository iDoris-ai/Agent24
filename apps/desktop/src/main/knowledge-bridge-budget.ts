// KB-C2-04b-1 (#1624): the bridge's own read budget (U7.2). One per view
// instance. It never shares the main conversation's budget.

export const BRIDGE_READ_CONCURRENCY = 2
export const BRIDGE_READ_QUEUE_MS = 10_000

export type BridgeBudgetResult<T> = { kind: 'done'; value: T } | { kind: 'overloaded' } | { kind: 'cancelled' }

interface Job<T> {
  task: (signal: AbortSignal) => Promise<T>
  signal: AbortSignal
  resolve: (result: BridgeBudgetResult<T>) => void
  onAbort: () => void
  enqueuedAt: number
  timer: unknown
  controller: AbortController
  queued: boolean
  running: boolean
  settled: boolean
  released: boolean
}

export class BridgeReadBudget {
  private readonly queue: Array<Job<unknown>> = []
  private readonly running = new Set<Job<unknown>>()
  private active = 0
  private destroyed = false

  constructor(private readonly deps: { now: () => number; setTimer: (fn: () => void, ms: number) => unknown; clearTimer: (h: unknown) => void }) {}

  /** FIFO. Starts at once when a slot is free. Queued past 10 s settles overloaded. */
  run<T>(task: (signal: AbortSignal) => Promise<T>, signal: AbortSignal): Promise<BridgeBudgetResult<T>> {
    if (this.destroyed || signal.aborted) return Promise.resolve({ kind: 'cancelled' })
    return new Promise((resolve) => {
      const job: Job<T> = {
        task,
        signal,
        resolve,
        onAbort: () => this.onAbort(job as Job<unknown>),
        enqueuedAt: this.deps.now(),
        timer: null,
        controller: new AbortController(),
        queued: true,
        running: false,
        settled: false,
        released: false,
      }
      signal.addEventListener('abort', job.onAbort)
      this.queue.push(job as Job<unknown>)
      this.pump()
      if (job.queued && !job.settled) this.arm(job as Job<unknown>)
    })
  }

  /** Abort everything still running or queued. Each settles cancelled. */
  destroy(): void {
    if (this.destroyed) return
    this.destroyed = true
    const jobs = [...this.queue, ...this.running]
    this.queue.length = 0
    for (const job of jobs) {
      job.queued = false
      if (job.running) job.controller.abort()
      this.settle(job, { kind: 'cancelled' })
    }
  }

  private pump(): void {
    while (this.active < BRIDGE_READ_CONCURRENCY && !this.destroyed) {
      const job = this.queue.shift()
      if (!job) return
      job.queued = false
      if (job.settled) continue
      this.clearTimer(job)
      job.running = true
      this.running.add(job)
      this.active++
      this.start(job)
    }
  }

  private start(job: Job<unknown>): void {
    if (job.signal.aborted || this.destroyed) {
      job.controller.abort()
      this.settle(job, { kind: 'cancelled' })
      this.release(job)
      return
    }
    job.task(job.controller.signal).then(
      (value) => { if (!job.settled) this.settle(job, { kind: 'done', value }) },
      () => { if (!job.settled) this.settle(job, { kind: 'cancelled' }) },
    ).finally(() => this.release(job))
  }

  private onAbort(job: Job<unknown>): void {
    if (job.settled) return
    if (job.queued) {
      const i = this.queue.indexOf(job)
      if (i >= 0) this.queue.splice(i, 1)
      job.queued = false
      this.settle(job, { kind: 'cancelled' })
      return
    }
    if (job.running) {
      job.controller.abort()
      this.settle(job, { kind: 'cancelled' })
    }
  }

  private arm(job: Job<unknown>): void {
    const remain = BRIDGE_READ_QUEUE_MS - (this.deps.now() - job.enqueuedAt)
    if (remain <= 0) {
      this.queueTimeout(job)
      return
    }
    job.timer = this.deps.setTimer(() => this.queueTimeout(job), remain)
  }

  private queueTimeout(job: Job<unknown>): void {
    if (!job.queued || job.settled || this.destroyed) return
    if (this.deps.now() - job.enqueuedAt < BRIDGE_READ_QUEUE_MS) {
      this.arm(job)
      return
    }
    const i = this.queue.indexOf(job)
    if (i >= 0) this.queue.splice(i, 1)
    job.queued = false
    this.settle(job, { kind: 'overloaded' })
  }

  private settle(job: Job<unknown>, result: BridgeBudgetResult<unknown>): void {
    if (job.settled) return
    job.settled = true
    job.signal.removeEventListener('abort', job.onAbort)
    this.clearTimer(job)
    job.resolve(result)
  }

  private release(job: Job<unknown>): void {
    if (job.released) return
    job.released = true
    if (job.running) {
      job.running = false
      this.running.delete(job)
      this.active--
    }
    if (!this.destroyed) this.pump()
  }

  private clearTimer(job: Job<unknown>): void {
    if (job.timer === null) return
    this.deps.clearTimer(job.timer)
    job.timer = null
  }
}
