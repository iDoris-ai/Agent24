// CORE-2 AgentEar S3: the desktop main process's presence long-poll loop, with
// fetch / dialog / clock / timers mocked.

import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  BACKOFF_MAX_MS,
  BACKOFF_START_MS,
  IDLE_MS,
  NOT_SUPPORTED_MS,
  PresencePoller,
  showPresenceDialog,
  parseChallenge,
  type PresenceChallenge,
  type PresenceDialogOptions,
  type PresencePollerDeps,
} from './presence-poller'
import { mt, resetMainLocaleForTest, setMainLocale, type MainLocale } from './main-i18n'

const NOW_MS = 1_800_000_000_000
const CONN = { port: 4321, authHeaders: { Authorization: 'Bearer host-token' } }

function challenge(over: Partial<PresenceChallenge> = {}): PresenceChallenge {
  return {
    challenge_id: 'c1',
    operation: 'module.admin',
    summary: 'Register module agentear',
    requester: { identifier: 'agent24', pid: 4242 },
    expires_at: NOW_MS / 1000 + 60,
    ...over,
  }
}

const json = (body: unknown, status = 200): Response =>
  new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } })

interface Call { url: string; init?: RequestInit }

/** A scripted daemon: `next` answers successive polls (then parks until aborted). */
function harness(opts: {
  polls: Array<() => Response | Promise<Response>>
  dialog?: (o: PresenceDialogOptions, s: AbortSignal) => Promise<{ response: number }>
  connection?: PresencePollerDeps['connection']
}) {
  const calls: Call[] = []
  const posts: Array<{ url: string; body: unknown; headers: Record<string, string> }> = []
  const sleeps: number[] = []
  const timers: Array<{ fn: () => void; ms: number; cancelled: boolean }> = []
  const dialogs: PresenceDialogOptions[] = []
  let pollIndex = 0
  const fetchMock = vi.fn(async (url: string | URL | Request, init?: RequestInit) => {
    const u = String(url)
    calls.push({ url: u, ...(init ? { init } : {}) })
    if (init?.method === 'POST') {
      posts.push({
        url: u,
        body: JSON.parse(String(init.body)),
        headers: init.headers as Record<string, string>,
      })
      return json({ state: 'decided' })
    }
    const next = opts.polls[pollIndex++]
    if (next) return next()
    // Park like a real long-poll until stop() aborts it.
    return new Promise<Response>((_, reject) => {
      init?.signal?.addEventListener('abort', () => reject(new Error('aborted')))
    })
  })
  const deps: PresencePollerDeps = {
    connection: opts.connection ?? (() => CONN),
    fetch: fetchMock as unknown as typeof fetch,
    showDialog: (o, s) => {
      dialogs.push(o)
      return (opts.dialog ?? (async () => ({ response: 1 })))(o, s)
    },
    t: mt,
    nowMs: () => NOW_MS,
    sleep: async (ms, signal) => {
      sleeps.push(ms)
      // Yield so a parked loop cannot starve the test, and honour stop().
      await new Promise<void>((resolve) => {
        if (signal.aborted) return resolve()
        const t = setTimeout(resolve, 1)
        signal.addEventListener('abort', () => { clearTimeout(t); resolve() })
      })
    },
    setTimer: (fn, ms) => {
      const rec = { fn, ms, cancelled: false }
      timers.push(rec)
      return () => { rec.cancelled = true }
    },
    log: () => {},
  }
  return { deps, calls, posts, sleeps, timers, dialogs, poller: new PresencePoller(deps) }
}

async function until(cond: () => boolean): Promise<void> {
  for (let i = 0; i < 500 && !cond(); i += 1) await new Promise((r) => setTimeout(r, 2))
  expect(cond()).toBe(true)
}

afterEach(() => resetMainLocaleForTest())

describe('presence poller', () => {
  it('polls with the host credential and answers "allow" when the user picks Allow', async () => {
    const h = harness({
      polls: [() => json(challenge()), () => new Response(null, { status: 204 })],
      dialog: async () => ({ response: 0 }),
    })
    h.poller.start()
    await until(() => h.posts.length === 1)
    await h.poller.stop()

    expect(h.calls[0]?.url).toBe('http://127.0.0.1:4321/api/v1/presence/challenges/next?wait=25')
    expect(h.calls[0]?.init?.headers).toEqual(CONN.authHeaders)
    expect(h.posts[0]).toMatchObject({
      url: 'http://127.0.0.1:4321/api/v1/presence/challenges/c1',
      body: { decision: 'allow' },
    })
    expect(h.posts[0]?.headers.Authorization).toBe('Bearer host-token')
  })

  it('answers "deny" for Deny, and the dialog defaults to and cancels with Deny (English by default)', async () => {
    const h = harness({
      polls: [() => json(challenge())],
      dialog: async () => ({ response: 1 }),
    })
    h.poller.start()
    await until(() => h.posts.length === 1)
    await h.poller.stop()

    expect(h.posts[0]?.body).toEqual({ decision: 'deny' })
    const opts = h.dialogs[0]!
    expect(opts.buttons).toEqual(['Allow', 'Deny'])
    expect(opts.defaultId).toBe(1)
    expect(opts.cancelId).toBe(1)
    expect(opts.buttons[opts.defaultId]).toBe('Deny')
    expect(opts.title).toBe('Confirm on this computer')
    expect(opts.message).toContain('agent24')
    expect(opts.message).toContain('module.admin')
    expect(opts.detail).toContain('Register module agentear')
  })

  it('uses the main-process language for the dialog', async () => {
    setMainLocale('zh')
    const h = harness({ polls: [() => json(challenge())] })
    h.poller.start()
    await until(() => h.dialogs.length === 1)
    await h.poller.stop()
    expect(h.dialogs[0]?.buttons).toEqual(['允许', '拒绝'])
  })

  it('denies when the dialog itself fails to open', async () => {
    const h = harness({
      polls: [() => json(challenge())],
      dialog: async () => { throw new Error('no window server') },
    })
    h.poller.start()
    await until(() => h.posts.length === 1)
    await h.poller.stop()
    expect(h.posts[0]?.body).toEqual({ decision: 'deny' })
  })

  it('closes the dialog at expires_at and never sends the late answer', async () => {
    let dialogSignal: AbortSignal | undefined
    let answer: (v: { response: number }) => void = () => {}
    const h = harness({
      polls: [() => json(challenge({ expires_at: NOW_MS / 1000 + 60 }))],
      dialog: (_o, signal) => {
        dialogSignal = signal
        return new Promise((resolve) => { answer = resolve })
      },
    })
    h.poller.start()
    await until(() => h.timers.length === 1)
    expect(h.timers[0]?.ms).toBe(60_000)

    h.timers[0]!.fn() // the clock reaches expires_at
    expect(dialogSignal?.aborted).toBe(true)
    answer({ response: 0 }) // user clicks Allow a moment too late
    await until(() => h.timers[0]?.cancelled === true)
    expect(h.posts).toEqual([])
    await h.poller.stop()
    expect(h.timers[0]?.cancelled).toBe(true)
  })

  it('drops Allow at actual expiry even when the expiry timer has not fired', async () => {
    let now = NOW_MS
    let answer: (v: { response: number }) => void = () => {}
    const h = harness({ polls: [() => json(challenge())], dialog: () => new Promise((resolve) => { answer = resolve }) })
    h.deps.nowMs = () => now
    h.poller.start()
    await until(() => h.dialogs.length === 1)
    now += 61_000
    answer({ response: 0 })
    await until(() => h.timers[0]?.cancelled === true)
    expect(h.posts).toEqual([])
    await h.poller.stop()
  })

  it('does not open a dialog for a challenge that has already expired', async () => {
    const h = harness({ polls: [() => json(challenge({ expires_at: NOW_MS / 1000 - 1 }))] })
    h.poller.start()
    await until(() => h.calls.length >= 2)
    await h.poller.stop()
    expect(h.dialogs).toEqual([])
    expect(h.posts).toEqual([])
  })

  it('stop() aborts the in-flight poll, closes an open dialog and sends nothing', async () => {
    let dialogSignal: AbortSignal | undefined
    const h = harness({
      polls: [() => json(challenge())],
      dialog: (_o, signal) => {
        dialogSignal = signal
        return new Promise((resolve) => signal.addEventListener('abort', () => resolve({ response: 1 })))
      },
    })
    h.poller.start()
    await until(() => h.dialogs.length === 1)
    const before = h.calls.length
    await h.poller.stop()

    expect(dialogSignal?.aborted).toBe(true)
    await new Promise((r) => setTimeout(r, 20))
    expect(h.posts).toEqual([])
    expect(h.calls.length).toBe(before)
  })

  it('keeps polling while the user is still deciding', async () => {
    const h = harness({
      polls: [() => json(challenge()), () => new Response(null, { status: 204 }), () => new Response(null, { status: 204 })],
      dialog: (_o, signal) => new Promise((resolve) => signal.addEventListener('abort', () => resolve({ response: 1 }))), // user has not answered
    })
    h.poller.start()
    await until(() => h.calls.length >= 3)
    await h.poller.stop()
    expect(h.calls.filter((c) => c.init?.method !== 'POST').length).toBeGreaterThanOrEqual(3)
  })

  it('asks one challenge at a time', async () => {
    const order: string[] = []
    const releases: Array<() => void> = []
    const h = harness({
      polls: [
        () => json(challenge({ challenge_id: 'a' })),
        () => json(challenge({ challenge_id: 'b' })),
      ],
      dialog: (o) => new Promise((resolve) => {
        order.push(o.message)
        releases.push(() => resolve({ response: 0 }))
      }),
    })
    h.poller.start()
    await until(() => h.calls.length >= 3)
    expect(order).toHaveLength(1)
    releases[0]!()
    await until(() => order.length === 2)
    releases[1]!()
    await until(() => h.posts.length === 2)
    await h.poller.stop()
    expect(h.posts.map((p) => p.url.split('/').pop())).toEqual(['a', 'b'])
  })

  it('backs off exponentially on errors, caps it, and resets after a good poll', async () => {
    const boom = (): Response => { throw new Error('ECONNREFUSED') }
    const h = harness({
      polls: [boom, boom, boom, () => new Response(null, { status: 204 }), boom],
    })
    h.poller.start()
    await until(() => h.sleeps.length >= 5)
    await h.poller.stop()
    const [a, b, c] = h.sleeps
    expect([a, b, c]).toEqual([BACKOFF_START_MS, BACKOFF_START_MS * 2, BACKOFF_START_MS * 4])
    expect(Math.max(...h.sleeps)).toBeLessThanOrEqual(BACKOFF_MAX_MS)
    // 204 reset the backoff (and, answered instantly, is paced by IDLE_MS).
    expect(h.sleeps[3]).toBe(IDLE_MS)
    expect(h.sleeps[4]).toBe(BACKOFF_START_MS)
  })

  it('waits a long time when the daemon has no presence routes (404, e.g. legacy mode)', async () => {
    const h = harness({ polls: [() => json({ error: { code: 'not_found' } }, 404)] })
    h.poller.start()
    await until(() => h.sleeps.length >= 1)
    await h.poller.stop()
    expect(h.sleeps[0]).toBe(NOT_SUPPORTED_MS)
  })

  it('does nothing without a credential (no daemon, or the unauthenticated node mock)', async () => {
    for (const connection of [() => null, () => ({ port: 8765, authHeaders: {} })]) {
      const h = harness({ polls: [() => json(challenge())], connection })
      h.poller.start()
      await until(() => h.sleeps.length >= 2)
      await h.poller.stop()
      expect(h.calls).toEqual([])
    }
  })

  it('resumes when the backend comes back after it stopped', async () => {
    let conn: ReturnType<PresencePollerDeps['connection']> = null
    const h = harness({ polls: [() => new Response(null, { status: 204 })], connection: () => conn })
    h.poller.start()
    await until(() => h.sleeps.length >= 1)
    expect(h.calls).toEqual([])
    conn = CONN
    await until(() => h.calls.length >= 1)
    await h.poller.stop()
  })

  it('stop() is final until start() is called again', async () => {
    const h = harness({ polls: [] })
    h.poller.start()
    await until(() => h.calls.length === 1)
    await h.poller.stop()
    const after = h.calls.length
    await new Promise((r) => setTimeout(r, 20))
    expect(h.calls.length).toBe(after)
  })
})

describe('parseChallenge', () => {
  it('accepts the daemon shape and rejects anything else', () => {
    expect(parseChallenge(challenge())).toEqual(challenge())
    for (const bad of [null, 'x', {}, { ...challenge(), challenge_id: '' }, { ...challenge(), expires_at: 'soon' }, { ...challenge(), requester: null }]) {
      expect(parseChallenge(bad)).toBeNull()
    }
  })
})

describe('presence dialog strings', () => {
  const keys = ['presence.title', 'presence.message', 'presence.detail', 'presence.allow', 'presence.deny'] as const

  it.each(['en', 'zh', 'th'] as MainLocale[])('has every key in %s with its placeholders', (locale) => {
    setMainLocale(locale)
    for (const key of keys) expect(mt(key).length).toBeGreaterThan(0)
    expect(mt('presence.message')).toMatch(/\{requester\}/)
    expect(mt('presence.message')).toMatch(/\{pid\}/)
    expect(mt('presence.message')).toMatch(/\{operation\}/)
    expect(mt('presence.detail')).toMatch(/\{summary\}/)
  })

  it('uses the shared main-process locale for presence and tray messages', () => {
    setMainLocale('zh')
    expect(mt('tray.show')).toBe('显示窗口')
    expect(mt('presence.deny')).toBe('拒绝')
    setMainLocale('th')
    expect(mt('tray.show')).toBe('แสดงหน้าต่าง')
    expect(mt('presence.deny')).toBe('ปฏิเสธ')
  })

  it('the three languages differ for the buttons, and English is the default', () => {
    expect(mt('presence.allow')).toBe('Allow')
    expect(mt('presence.deny')).toBe('Deny')
    expect(new Set((['en', 'zh', 'th'] as MainLocale[]).map((l) => { setMainLocale(l); return mt('presence.deny') })).size).toBe(3)
  })
})

describe('trusted presence dialog parent', () => {
  const opts: PresenceDialogOptions = { type: 'question', title: '', message: '', detail: '', buttons: [], defaultId: 1, cancelId: 1, noLink: true }

  it('shows and focuses a hidden main window before opening its cancellable sheet', async () => {
    const events: string[] = []
    const parent = {
      isDestroyed: () => false, isVisible: () => false, isMinimized: () => true,
      restore: () => { events.push('restore') }, show: () => { events.push('show') }, focus: () => { events.push('focus') },
    }
    const signal = new AbortController().signal
    await showPresenceDialog(parent, opts, signal, async (actualParent, actualOpts) => {
      expect(actualParent).toBe(parent)
      expect(actualOpts.signal).toBe(signal)
      expect(events).toEqual(['restore', 'show', 'focus'])
      return { response: 1 }
    })
  })

  it.each([null, { isDestroyed: () => true, isMinimized: () => false, restore: vi.fn(), show: vi.fn(), focus: vi.fn() }])('never opens a parentless dialog for an unavailable window: %s', async (parent) => {
    const showBox = vi.fn(async () => ({ response: 0 }))
    await expect(showPresenceDialog(parent, opts, new AbortController().signal, showBox)).rejects.toThrow()
    expect(showBox).not.toHaveBeenCalled()
  })
})
