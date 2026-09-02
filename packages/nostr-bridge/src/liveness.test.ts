// FU-32: the bridge MUST be able to tell "收件箱为空" from "入站通路已死".
//
// The failure this file pins down is the one that survives every timeout: the
// daemon's relay path breaks, `history inbox` keeps exiting 0 with `[]`, nothing
// throws, no child hangs, `pollOnce` returns normally — and the bridge answers
// nobody for days while looking perfectly healthy. `no_signal_at_all_is_silent`
// below is that bug, asserted as the baseline the probe has to beat.
//
// The tests drive the REAL SpeakerClient / InboundLiveness / pollOnce through
// FakeSpeaker, and model the agent-speaker daemon the way the real one behaves
// (verified against hyphae @98834db): a sent message is stored locally with
// `is_incoming=0`, and only when the DAEMON pulls that same event back off the
// relay does `INSERT OR REPLACE` flip the row to `is_incoming=1`. `deliver()` is
// the daemon doing its job; not calling it is the daemon's relay path being dead.
//
// MUTATION CHECKS — each verified by hand to turn the NAMED test red:
//   observe()
//     · drop the `outstanding.delete` guard, confirm on any self-row
//         → 一条 canary 只确认一次   (a stale row would re-prove liveness forever)
//     · confirm on the event id alone, without comparing content
//         → 解密链路坏了            (the daemon stores what it cannot decrypt)
//     · do not clear the send backoff on a confirmation
//         → queued 的 canary 后来送达并确认
//   nextDelay()
//     · always return the base interval        → 上游竞态:发布早于写出站行
//     · count every outstanding, not only published → 只剩没发出去的那条时不加速
//   armCanary()
//     · round the delay up to a multiple of tickMs → 发送时刻不被 30s tick 量化
//         THIS IS THE BUG THE FIRST ATTEMPT HAD: a jittered deadline checked on
//         a fixed tick still fires at t0 + N×30s, so the phase never drifts.
//     · drop the final clamp                   → 递给 setTimeout 的延迟不能越界
//   restore()
//     · drop any field validation              → 损坏的健康文件…
//     · do not carry the counters              → 计数跨重启累计
//     · swallow every read error, not just ENOENT → 读不了快照…
//   elapsed()
//     · drop restoredGapMs                     → 反复重启不能刷掉静默
//   start()
//     · drop the idempotence guard             → 重复 start()…
//         Counting sends would NOT catch it (armCanary clears its own handle);
//         the leak is a second judge-tick chain, so the test counts inbox reads.
//   pollOnce()
//     · pass the raw rows instead of observe() → 走真实 pollOnce
//
// Three of these tests did NOT discriminate when first written (they asserted
// through a path the mutation did not touch, or the fixture never built the
// state under test). If a mutation here stops failing, suspect the test first.

import { describe, it, expect, vi } from 'vitest'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { InboundLiveness, CANARY_PREFIX } from './liveness.js'
import { durationMs } from './config.js'
import { SpeakerClient, SpeakerTimeoutError, type SpeakerRunner } from './speaker.js'
import { InboundBridge, pollOnce } from './inbound.js'
import type { Agent24Client } from './agent24.js'
import { FakeSpeaker } from './testing/fake-speaker.js'

/** Reach the private scheduling decision — the thing that decides whether an
 * outage becomes a flood. Asserting it directly beats asserting a timer's shape. */
function nextDelayOf(l: InboundLiveness): number {
  return (l as unknown as { nextDelay: () => number }).nextDelay()
}

const SELF = 'npub1self'
const CANARY_MS = 60_000
// Must clear the constructor's floor (2 × canary + 60s) or it gets clamped.
const STALE_MS = 200_000

function harness(
  opts: {
    staleAfterMs?: number
    canaryIntervalMs?: number
    healthFile?: string
    /** Model the upstream store race: the daemon writes is_incoming=1 first and
     * the sending CLI then REPLACEs the row back to 0, losing the canary on a
     * healthy system. See the module header / FU-33. */
    loseCanaries?: boolean
    /** Model a broken daemon decryption path: the row comes back under the right
     * event id, but carrying ciphertext instead of what we sent. */
    garbleContent?: boolean
    wallStart?: number
  } = {},
) {
  const fake = new FakeSpeaker()
  let t = 0
  let seq = 0
  /** Canaries that have been published but not yet pulled back by the daemon. */
  const inFlight: { id: string; content: string }[] = []

  const runner: SpeakerRunner = async (args) => {
    if (args[0] === 'agent' && args[1] === 'msg') {
      // A real event id is 64 hex chars. The shape matters: `inbox()` only
      // treats an id as the real event id when it matches /^[0-9a-f]{16,}$/,
      // otherwise it synthesizes a dedup key — and a synthesized key would
      // never match the id `agent msg` handed back, so the canary could never
      // confirm. A fixture with a fake-looking id hides that.
      const id = (++seq).toString(16).padStart(64, '0')
      const ci = args.indexOf('--content')
      inFlight.push({ id, content: ci >= 0 ? args[ci + 1]! : '' })
      fake.sendResult = { event_id: id, published_to: 1, relay_count: 1, queued_for_retry: false }
    }
    return fake.runner(args)
  }

  const speaker = new SpeakerClient(runner, { identity: 'agent24' })
  const logs: { level: string; text: string }[] = []
  const log = {
    log: (...a: unknown[]) => logs.push({ level: 'log', text: a.join(' ') }),
    warn: (...a: unknown[]) => logs.push({ level: 'warn', text: a.join(' ') }),
    error: (...a: unknown[]) => logs.push({ level: 'error', text: a.join(' ') }),
  }

  const wallStart = opts.wallStart ?? 1_700_000_000_000
  const liveness = new InboundLiveness({
    speaker,
    identity: 'agent24',
    resolveSelfNpub: () => speaker.npubFor('agent24'),
    canaryIntervalMs: opts.canaryIntervalMs ?? CANARY_MS,
    staleAfterMs: opts.staleAfterMs ?? STALE_MS,
    healthFile: opts.healthFile,
    now: () => t,
    wallNow: () => wallStart + t,
    rand: () => 0.5, // no jitter, so the tests' timing arithmetic stays exact
    log,
  })

  return {
    fake,
    speaker,
    liveness,
    logs,
    inFlight,
    advance: (ms: number) => {
      t += ms
    },
    /** Resolve the npub and read any previous health file, then cancel the
     * probe's real timer — the tests drive `beat()` by hand. */
    boot: async () => {
      await liveness.start()
      liveness.stop()
    },
    /** The daemon pulls the in-flight canaries off the relay: the local row
     * flips to is_incoming=1 and becomes visible to `history inbox`. */
    deliver: () => {
      for (const c of inFlight.splice(0)) {
        if (opts.loseCanaries) continue // the row exists, but stuck at is_incoming=0
        fake.inboxRows.push({
          id: c.id,
          sender_npub: SELF,
          plaintext: opts.garbleContent ? 'AhkP2s+ciphertext-we-cannot-read' : c.content,
          created_at: Math.floor(t / 1000),
          is_incoming: true,
        })
      }
    },
    /** Send one canary (the probe's own timer does this in production). */
    probe: () => liveness.probe(),
    /** Read the inbox, confirm, judge, persist — no sending. */
    judge: () => liveness.beat(),
    /** A canary going out and then a judgement, which is what one round of the
     * probe amounts to. They run on SEPARATE timers in production (see
     * `armCanary`), so tests that care about that drive them apart. */
    cycle: async () => {
      await liveness.probe()
      await liveness.beat()
    },
  }
}

describe('InboundLiveness — 区分「收件箱为空」和「入站通路已死」', () => {
  it('no_signal_at_all_is_silent: 没有探针时,死掉的入站通路和空收件箱完全一样', async () => {
    // The baseline bug. The daemon is dead (deliver() is never called), so the
    // inbox is empty forever — and every observable the bridge had before FU-32
    // says "healthy": no throw, no timeout, exit 0, empty list.
    const h = harness()
    const rows = await h.speaker.inbox()
    expect(rows).toEqual([]) // identical to a genuinely quiet inbox
    // ...and identical to this, which IS a quiet inbox with a live daemon:
    h.deliver()
    expect(await h.speaker.inbox()).toEqual([])
  })

  it('通路健康:canary 往返 → ok', async () => {
    const h = harness()
    await h.boot()
    await h.cycle() // sends canary #1
    expect(h.liveness.current).toBe('starting')

    h.deliver() // the daemon does its job
    h.advance(1_000)
    await h.cycle()

    expect(h.liveness.current).toBe('ok')
    expect(h.logs.some((l) => l.text.includes('入站通路已确认'))).toBe(true)
  })

  it('通路已死:canary 一去不回 → 到点报死(这就是 FU-32)', async () => {
    const h = harness()
    await h.cycle()
    expect(h.liveness.current).toBe('starting')

    // The inbox stays empty the whole time — exactly the state the pre-FU-32
    // bridge called healthy.
    h.advance(STALE_MS + 1)
    await h.cycle()

    expect(h.liveness.current).toBe('degraded')
    const err = h.logs.find((l) => l.level === 'error')
    expect(err?.text).toContain('入站通路疑似已死')
    // The log has to be actionable, not just loud: the three real causes.
    expect(err?.text).toContain('daemon')
    expect(err?.text).toContain('--relay')
  })

  it('daemon 根本没起:从未确认过也必须到点报死,不能永远停在 starting', async () => {
    // resolveSelfNpub works, canaries send fine, nothing ever comes back —
    // the shape of "operator forgot to start agent-speaker daemon".
    const h = harness()
    h.advance(STALE_MS + 1)
    await h.cycle()
    expect(h.liveness.current).toBe('degraded')
  })

  it('恢复:通路回来后 degraded → ok,并且报死后仍然继续发 canary', async () => {
    const h = harness()
    await h.cycle()
    h.advance(STALE_MS + 1)
    await h.cycle()
    expect(h.liveness.current).toBe('degraded')

    // A probe that stops probing once it despairs can never recover.
    const sentWhileDegraded = h.inFlight.length
    expect(sentWhileDegraded).toBeGreaterThan(0)

    h.deliver()
    h.advance(1_000)
    await h.cycle()

    expect(h.liveness.current).toBe('ok')
    // The recovery line must report the OUTAGE, not the time since the canary
    // that ended it. Computing it from `lastConfirmedAt` (just moved to now)
    // prints "中断约 0s" for every outage however long — useless in a log an
    // operator reads days later to find out how long they were off the air.
    const recovered = h.logs.find((l) => l.text.includes('入站通路已恢复'))!
    expect(recovered).toBeDefined()
    const seconds = Number(/中断约 (\d+)s/.exec(recovered.text)![1])
    expect(seconds).toBeGreaterThanOrEqual(STALE_MS / 1000)
  })
})

describe('InboundLiveness — 什么不算证据(fail-closed)', () => {
  it('一条 canary 只确认一次:留在窗口里的旧行不能反复证明活着', async () => {
    const h = harness()
    await h.cycle()
    h.deliver()
    h.advance(1_000)
    await h.cycle()
    expect(h.liveness.current).toBe('ok')

    // The canary row is still in the inbox window and will be re-read on every
    // poll. If it could confirm again, a bridge whose path died five minutes
    // later would report `ok` forever — the exact fail-open FU-32 warns about.
    h.advance(STALE_MS + 1)
    await h.cycle()
    expect(h.liveness.current).toBe('degraded')
  })

  it('上一轮进程留下的自发消息:丢弃,但绝不算确认', async () => {
    const h = harness()
    // A canary from an earlier run — right sender, right marker, id we never saw.
    h.fake.inboxRows.push({
      id: 'ffff'.repeat(16), // a real-shaped id we never sent
      sender_npub: SELF,
      plaintext: `${CANARY_PREFIX} stale-token`,
      created_at: 0,
      is_incoming: true,
    })
    await h.cycle() // resolves selfNpub as a side effect of the first canary send
    h.advance(STALE_MS + 1)
    await h.cycle()
    expect(h.liveness.current).toBe('degraded')
  })

  it('canary 原路返回但内容不对(解密链路坏了)= 不是确认', async () => {
    // The daemon calls StoreIncomingMessage even when NIP-44 decryption fails
    // (daemon.go:358-368), so the row comes back under the right event id
    // carrying ciphertext. Matching on the id alone would call that healthy —
    // while real peer messages are reaching the agent as unreadable ciphertext.
    const h = harness({ garbleContent: true })
    await h.boot()
    await h.cycle()
    h.deliver()
    h.advance(1_000)
    await h.cycle()

    expect(h.liveness.current).not.toBe('ok')
    expect(h.liveness.snapshot().last_error).toContain('解密')
    h.advance(STALE_MS + 1)
    await h.cycle()
    expect(h.liveness.current).toBe('degraded')
  })

  it('canary 发送超时被杀 = 状态未知,不是确认', async () => {
    const fake = new FakeSpeaker()
    let t = 0
    const speaker = new SpeakerClient(
      async (args) => {
        if (args[0] === 'agent' && args[1] === 'msg') {
          throw new SpeakerTimeoutError('agent-speaker agent timed out after 60000ms and was killed')
        }
        return fake.runner(args)
      },
      { identity: 'agent24' },
    )
    const liveness = new InboundLiveness({
      speaker,
      identity: 'agent24',
      resolveSelfNpub: () => speaker.npubFor('agent24'),
      canaryIntervalMs: CANARY_MS,
      staleAfterMs: STALE_MS,
      now: () => t,
      log: { log: () => {}, warn: () => {}, error: () => {} },
    })

    await liveness.probe()
    await liveness.beat()
    t += STALE_MS + 1
    await liveness.probe()
    await liveness.beat()

    expect(liveness.current).toBe('degraded')
    expect(liveness.snapshot().last_error).toMatch(/超时被杀|状态未知/)
  })

  it('npub 解析不了:记录原因并到点报死,不是静默不探测', async () => {
    const fake = new FakeSpeaker()
    fake.identities = [] // the configured identity does not exist in the keystore
    let t = 0
    const speaker = new SpeakerClient(fake.runner, { identity: 'agent24' })
    const liveness = new InboundLiveness({
      speaker,
      identity: 'agent24',
      resolveSelfNpub: () => speaker.npubFor('agent24'),
      canaryIntervalMs: CANARY_MS,
      staleAfterMs: STALE_MS,
      now: () => t,
      log: { log: () => {}, warn: () => {}, error: () => {} },
    })

    await liveness.probe()
    expect(liveness.snapshot().last_error).toContain('npub')
    t += STALE_MS + 1
    await liveness.beat()
    expect(liveness.current).toBe('degraded')
  })
})

describe('InboundLiveness — 已知的上游竞态(FU-33)必须是可幸存的', () => {
  it('上游竞态:发布早于写出站行 → 丢一条 canary,但一条不足以报死', async () => {
    // hyphae publishes BEFORE storing the outgoing row (agent.go:208-238). If
    // the daemon is inside its 3s subscribe window it writes is_incoming=1 first
    // and the sending CLI then REPLACEs the row back to 0 — while the daemon's
    // `seen` set now holds the id and will never revisit it. The canary is lost
    // on a perfectly healthy system, so a single loss must not be able to walk
    // us to the threshold.
    //
    // Driven through the REAL timers: calling `probe()` by hand would pass even
    // if `canaryTurn` ignored `nextDelay()` entirely, which is the wiring that
    // actually decides whether the re-probe happens.
    vi.useFakeTimers()
    try {
      const fake = new FakeSpeaker()
      let seq = 0
      const sends: number[] = []
      const speaker = new SpeakerClient(async (args) => {
        if (args[0] === 'agent' && args[1] === 'msg') {
          sends.push(Date.now())
          fake.sendResult = {
            event_id: (++seq).toString(16).padStart(64, '0'),
            published_to: 1, // published, but the daemon never gives it back
          }
        }
        return fake.runner(args)
      }, { identity: 'agent24' })
      const liveness = new InboundLiveness({
        speaker,
        identity: 'agent24',
        resolveSelfNpub: () => speaker.npubFor('agent24'),
        canaryIntervalMs: 300_000,
        staleAfterMs: 900_000,
        tickMs: 30_000,
        now: () => Date.now(),
        rand: () => 0.5,
        log: { log: () => {}, warn: () => {}, error: () => {} },
      })
      await liveness.start()
      // One base interval. With the accelerated re-probe (base/3) this must fit
      // more than one send; without it, exactly one.
      await vi.advanceTimersByTimeAsync(300_000)
      liveness.stop()
      expect(sends.length).toBeGreaterThan(1)
    } finally {
      vi.useRealTimers()
    }
  })

  it('queued 的 canary 后来送达并确认 → 必须解除退避,不能周期性误报', async () => {
    // The absorbing state: a canary that missed the relay sets the failure
    // counter, agent-speaker's outbox publishes it later, it comes back and
    // confirms — but if the confirmation does not clear the backoff, the next
    // probe is 20-30 minutes out while the staleness window is 15, so a path
    // that demonstrably works reports degraded on a cycle.
    const fake = new FakeSpeaker()
    let t = 0
    let seq = 0
    const queued: { id: string; content: string }[] = []
    const speaker = new SpeakerClient(async (args) => {
      if (args[0] === 'agent' && args[1] === 'msg') {
        const id = (++seq).toString(16).padStart(64, '0')
        const ci = args.indexOf('--content')
        queued.push({ id, content: args[ci + 1]! })
        fake.sendResult = { event_id: id, published_to: 0, queued_for_retry: true }
      }
      return fake.runner(args)
    }, { identity: 'agent24' })
    const liveness = new InboundLiveness({
      speaker,
      identity: 'agent24',
      resolveSelfNpub: () => speaker.npubFor('agent24'),
      canaryIntervalMs: CANARY_MS,
      staleAfterMs: STALE_MS,
      now: () => t,
      rand: () => 0.5,
      log: { log: () => {}, warn: () => {}, error: () => {} },
    })
    await liveness.start()
    liveness.stop()

    await liveness.probe() // never reached a relay → backoff arms
    expect(nextDelayOf(liveness)).toBeGreaterThan(CANARY_MS)

    // The outbox publishes it; the daemon pulls it back.
    for (const c of queued) {
      fake.inboxRows.push({
        id: c.id,
        sender_npub: SELF,
        plaintext: c.content,
        created_at: 1,
        is_incoming: true,
      })
    }
    t += 1_000
    await liveness.beat()

    expect(liveness.current).toBe('ok')
    // Back to the normal cadence — well inside the staleness window.
    expect(nextDelayOf(liveness)).toBeLessThan(STALE_MS)
    expect(nextDelayOf(liveness)).toBeLessThanOrEqual(Math.round(CANARY_MS * 1.2))
  })
})

describe('InboundLiveness — 探针不能污染业务路径', () => {
  it('canary 不会被当成对端消息处理', async () => {
    const h = harness()
    await h.cycle()
    h.deliver()

    const rows = await h.speaker.inbox()
    expect(rows).toHaveLength(1) // the canary IS in the inbox
    const peers = h.liveness.observe(rows)
    expect(peers).toHaveLength(0) // ...and it is not peer traffic
  })

  it('走真实 pollOnce:canary 到不了 InboundBridge,对端消息到得了', async () => {
    // The wiring, not just `observe()` in isolation. If `pollOnce` handed the
    // raw rows to the bridge, every poll would either log the canary as an
    // unauthorized sender forever, or — for an operator who allowlisted their
    // own npub — make the agent answer itself in a loop.
    const h = harness()
    await h.cycle()
    h.deliver()
    h.fake.inboxRows.push({
      id: 'abcd'.repeat(16),
      sender_npub: 'npub1peer',
      plaintext: 'hello',
      created_at: 2,
      is_incoming: true,
    })

    const prompts: string[] = []
    const agent = {
      async createSession(): Promise<string> {
        return 'sess-1'
      },
      async runToCompletion(prompt: string) {
        prompts.push(prompt)
        return { status: 'completed', text: 'ok', runId: 'r1' }
      },
    } as unknown as Agent24Client
    // Both npubs are allowlisted, so nothing but `observe()` can keep the
    // canary out of the run path.
    const bridge = new InboundBridge(
      agent,
      h.speaker,
      'agent24',
      new Set([SELF, 'npub1peer']),
    )

    await pollOnce(h.speaker, bridge, h.liveness)

    expect(prompts).toEqual(['hello'])
    expect(prompts.some((p) => p.includes(CANARY_PREFIX))).toBe(false)
  })

  it('授权对端发以 marker 开头的消息不会被吞掉', async () => {
    // Recognising our own traffic by its marker text (rather than by sender)
    // would let any peer silence themselves by prefixing their message.
    const h = harness()
    await h.boot()
    h.fake.inboxRows.push({
      id: 'beef'.repeat(16),
      sender_npub: 'npub1peer',
      plaintext: `${CANARY_PREFIX} 我就是要用这个前缀说话`,
      created_at: 3,
      is_incoming: true,
    })
    const peers = h.liveness.observe(await h.speaker.inbox())
    expect(peers.map((p) => p.from)).toEqual(['npub1peer'])
  })

  it('发送与判定在不同的 timer 上:canary 卡死也不挡判定和健康文件', async () => {
    // Driven through `start()` and the real timer chain: calling `probe()` and
    // `beat()` by hand would stay green even if production went back to a single
    // timer, which is the thing under test.
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-liveness-'))
    const file = path.join(dir, 'health.json')
    vi.useFakeTimers()
    try {
      const fake = new FakeSpeaker()
      let release!: () => void
      const blocked = new Promise<void>((r) => {
        release = r
      })
      const speaker = new SpeakerClient(
        async (args) => {
          if (args[0] === 'agent' && args[1] === 'msg') {
            await blocked // the canary send hangs for the whole test
            return fake.runner(args)
          }
          return fake.runner(args)
        },
        { identity: 'agent24' },
      )
      const liveness = new InboundLiveness({
        speaker,
        identity: 'agent24',
        resolveSelfNpub: () => speaker.npubFor('agent24'),
        canaryIntervalMs: 300_000,
        staleAfterMs: 900_000,
        tickMs: 30_000,
        healthFile: file,
        now: () => Date.now(),
        rand: () => 0.5,
        log: { log: () => {}, warn: () => {}, error: () => {} },
      })

      await liveness.start()
      // Past the staleness window, with the first canary still hanging. Advanced
      // by a whole extra tick: the judge only runs ON a tick, and the tick that
      // lands exactly on 900_000 sees `elapsed > staleAfterMs` as false.
      await vi.advanceTimersByTimeAsync(930_000)
      liveness.stop()

      expect(liveness.current).toBe('degraded')
      // And the verdict reached the file an operator reads, not just memory.
      expect((JSON.parse(fs.readFileSync(file, 'utf8')) as { state: string }).state).toBe('degraded')
      release()
    } finally {
      vi.useRealTimers()
      fs.rmSync(dir, { recursive: true, force: true })
    }
  })

  it('对端消息原样放行', async () => {
    const h = harness()
    await h.cycle()
    h.fake.inboxRows.push({
      id: 'abcd'.repeat(16),
      sender_npub: 'npub1peer',
      plaintext: 'hello',
      created_at: 1,
      is_incoming: true,
    })
    const peers = h.liveness.observe(await h.speaker.inbox())
    expect(peers.map((p) => p.from)).toEqual(['npub1peer'])
  })

  it('未被 daemon 拉回来的自发行(is_incoming=0)对桥不可见 —— 这就是判据本身', async () => {
    // The whole discriminator: the outgoing row exists the moment we send, so
    // if `inbox()` returned it the canary would confirm liveness WITHOUT ever
    // touching a relay — a probe that proves nothing.
    const h = harness()
    await h.cycle()
    h.fake.inboxRows.push({
      id: h.inFlight[0]!.id,
      sender_npub: SELF,
      plaintext: h.inFlight[0]!.content,
      created_at: 0,
      is_incoming: false, // StoreOutgoingMessage
    })
    expect(await h.speaker.inbox()).toEqual([])
    h.advance(STALE_MS + 1)
    await h.cycle()
    expect(h.liveness.current).toBe('degraded')
  })
})

describe('InboundLiveness — 运维面', () => {
  it('健康快照写成原子文件,状态和计数可读', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-liveness-'))
    const file = path.join(dir, 'nested', 'health.json')
    try {
      const h = harness({ healthFile: file })
      await h.cycle()
      h.deliver()
      h.advance(1_000)
      await h.cycle()

      const snap = JSON.parse(fs.readFileSync(file, 'utf8')) as Record<string, unknown>
      expect(snap.state).toBe('ok')
      expect((snap.canaries as { confirmed: number }).confirmed).toBe(1)
      expect(snap.last_confirmed_at).toBeTruthy()
      expect(fs.existsSync(`${file}.tmp`)).toBe(false) // renamed, not left behind

      h.advance(STALE_MS + 1)
      await h.cycle()
      expect(
        (JSON.parse(fs.readFileSync(file, 'utf8')) as Record<string, unknown>).state,
      ).toBe('degraded')
    } finally {
      fs.rmSync(dir, { recursive: true, force: true })
    }
  })

  it('反复重启不能刷掉静默:supervisor 每几分钟拉一次也必须报死', async () => {
    // The soak criterion is "state never became degraded". If each new process
    // started its own grace period, a bridge that a supervisor restarts inside
    // that window could never reach degraded — and a week with the inbound path
    // dead the whole time would report a clean PASS.
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-liveness-'))
    const file = path.join(dir, 'health.json')
    try {
      let wall = 1_700_000_000_000
      for (let restart = 0; restart < 4; restart++) {
        const h = harness({ healthFile: file, wallStart: wall })
        await h.boot()
        await h.cycle() // sends a canary; the daemon never returns it
        h.advance(60_000) // this process lives one minute, then is restarted
        await h.cycle()
        wall += 60_000
        if (restart === 3) {
          // 4 minutes of process time, but ~4 minutes of accumulated silence too.
          expect(h.liveness.snapshot().seconds_since_confirmed).toBeGreaterThanOrEqual(180)
        }
      }
      // One more restart, this time after enough total silence to cross it.
      wall += STALE_MS
      const last = harness({ healthFile: file, wallStart: wall })
      await last.boot()
      await last.cycle()
      expect(last.liveness.current).toBe('degraded')
    } finally {
      fs.rmSync(dir, { recursive: true, force: true })
    }
  })

  it('重启继承的是静默,不是报警:上一轮健康的话新进程不误报', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-liveness-'))
    const file = path.join(dir, 'health.json')
    try {
      const first = harness({ healthFile: file })
      await first.boot()
      await first.cycle()
      first.deliver()
      first.advance(1_000)
      await first.cycle()
      expect(first.liveness.current).toBe('ok')

      // Restarted 30s later — well inside the window.
      const second = harness({ healthFile: file, wallStart: 1_700_000_000_000 + 31_000 })
      await second.boot()
      await second.cycle()
      expect(second.liveness.current).not.toBe('degraded')
    } finally {
      fs.rmSync(dir, { recursive: true, force: true })
    }
  })

  it('陈旧阈值小于两个 canary 周期必然误报 → 抬到下限并警告', () => {
    const warn = vi.fn()
    const fake = new FakeSpeaker()
    const speaker = new SpeakerClient(fake.runner, { identity: 'agent24' })
    new InboundLiveness({
      speaker,
      identity: 'agent24',
      resolveSelfNpub: () => speaker.npubFor('agent24'),
      canaryIntervalMs: 60_000,
      staleAfterMs: 1_000,
      now: () => 0,
      log: { log: () => {}, warn, error: () => {} },
    })
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('必然误报'))
  })

  it('健康文件写不进去不会拖垮桥', async () => {
    // A directory where the file should be: rename fails every time.
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-liveness-'))
    const file = path.join(dir, 'health.json')
    fs.mkdirSync(file)
    try {
      const h = harness({ healthFile: file })
      await expect(h.cycle()).resolves.toBeUndefined()
      expect(h.logs.some((l) => l.level === 'warn' && l.text.includes('健康快照'))).toBe(true)
    } finally {
      fs.rmSync(dir, { recursive: true, force: true })
    }
  })
})

describe('InboundLiveness — 发送相位必须真的会漂移(H-1)', () => {
  /** Build a probe on FAKE timers and record the wall time of every canary. */
  function phaseHarness(rands: number[]) {
    const fake = new FakeSpeaker()
    const sentAt: number[] = []
    let i = 0
    const speaker = new SpeakerClient(
      async (args) => {
        if (args[0] === 'agent' && args[1] === 'msg') {
          sentAt.push(Date.now())
          fake.sendResult = { event_id: (sentAt.length + 1).toString(16).padStart(64, '0'), published_to: 1 }
        }
        return fake.runner(args)
      },
      { identity: 'agent24' },
    )
    const liveness = new InboundLiveness({
      speaker,
      identity: 'agent24',
      resolveSelfNpub: () => speaker.npubFor('agent24'),
      canaryIntervalMs: 300_000, // 5 min — an exact multiple of the daemon's 30s
      staleAfterMs: 900_000,
      tickMs: 30_000,
      now: () => Date.now(),
      rand: () => rands[i++ % rands.length]!,
      log: { log: () => {}, warn: () => {}, error: () => {} },
    })
    return { liveness, sentAt, inboxReads: () => fake.calls.filter((c) => c.args[0] === 'history').length }
  }

  it('canary 发送时刻不被 30s tick 量化 —— 否则抖动等于没做', async () => {
    // The bug this pins: checking a jittered deadline on a fixed 30s tick still
    // fires at t0 + N×30s, so `mod 30s` never moves and the probe stays locked
    // to whatever phase it started in — including, possibly, one that sits
    // inside the daemon's 3s subscribe window and loses EVERY canary.
    vi.useFakeTimers()
    try {
      const { liveness, sentAt } = phaseHarness([0.13, 0.87, 0.41, 0.66, 0.29])
      await liveness.start()
      for (let n = 0; n < 6; n++) await vi.advanceTimersByTimeAsync(300_000)
      liveness.stop()

      expect(sentAt.length).toBeGreaterThanOrEqual(4)
      const phases = new Set(sentAt.map((ms) => ms % 30_000))
      // Quantized scheduling collapses every send onto the same residue.
      expect(phases.size).toBeGreaterThan(1)
    } finally {
      vi.useRealTimers()
    }
  })

  it('重复 start() 不会留下两条自我调度链', async () => {
    // Counting SENDS would not catch this: `armCanary` clears the previous
    // handle, so the canary chain self-heals. The leak is the JUDGE tick — two
    // chains, each spawning its own `history inbox` subprocess forever.
    vi.useFakeTimers()
    try {
      const twice = phaseHarness([0.5])
      await twice.liveness.start()
      await twice.liveness.start() // must be a no-op
      await vi.advanceTimersByTimeAsync(300_000)
      twice.liveness.stop()

      const once = phaseHarness([0.5])
      await once.liveness.start()
      await vi.advanceTimersByTimeAsync(300_000)
      once.liveness.stop()

      expect(twice.inboxReads()).toBe(once.inboxReads())
    } finally {
      vi.useRealTimers()
    }
  })
})

describe('InboundLiveness — 定时器边界(M-4)', () => {
  it('间隔顶到上限时,递给 setTimeout 的延迟不能越界', async () => {
    // durationMs() clamps the CONFIG to 2^31-1, but jitter multiplies by up to
    // 1.2 afterwards — and Node silently turns an out-of-range delay into 1ms:
    // the tight loop the config validation existed to prevent, reached by
    // arithmetic downstream of it. So the clamp has to live where the timer is
    // actually armed.
    //
    // Asserted on the DELAY handed to setTimeout rather than on observed firing,
    // because the fake timer library does not reproduce Node's overflow
    // coercion — a behavioural test here would pass no matter what.
    vi.useFakeTimers()
    const delays: number[] = []
    const spy = vi.spyOn(globalThis, 'setTimeout').mockImplementation(((
      fn: () => void,
      ms?: number,
    ) => {
      delays.push(ms ?? 0)
      return 0 as unknown as ReturnType<typeof setTimeout>
    }) as typeof setTimeout)
    try {
      const fake = new FakeSpeaker()
      const speaker = new SpeakerClient(fake.runner, { identity: 'agent24' })
      const liveness = new InboundLiveness({
        speaker,
        identity: 'agent24',
        resolveSelfNpub: () => speaker.npubFor('agent24'),
        canaryIntervalMs: 2 ** 31 - 1, // the ceiling durationMs allows
        staleAfterMs: 2 ** 31 - 1,
        tickMs: 30_000,
        now: () => Date.now(),
        rand: () => 0.99, // ×1.196 — over the signed-32-bit timer range
        log: { log: () => {}, warn: () => {}, error: () => {} },
      })
      await liveness.start()
      // Re-arm at the FULL jittered interval. No probe first: an outstanding
      // published canary would put `nextDelay` in the accelerated branch, whose
      // value is a third of the interval and never trips the ceiling — the test
      // would then pass regardless of the clamp.
      ;(liveness as unknown as { armCanary: (d: number) => void }).armCanary(
        (liveness as unknown as { nextDelay: () => number }).nextDelay(),
      )
      liveness.stop()

      expect(delays.length).toBeGreaterThan(0)
      for (const d of delays) {
        expect(Number.isFinite(d)).toBe(true)
        expect(d).toBeGreaterThanOrEqual(0)
        expect(d).toBeLessThanOrEqual(2 ** 31 - 1)
      }
    } finally {
      spy.mockRestore()
      vi.useRealTimers()
    }
  })
})

describe('InboundLiveness — 断网时不能自己制造洪水(M-1)', () => {
  it('canary 一条都没发出去时不加速重探,而是退避', async () => {
    // Every unpublished canary lands in agent-speaker's outbox and is retried
    // from there. Treating "unconfirmed" as "probe harder" during a week-long
    // outage would queue thousands of messages to fix nothing.
    const fake = new FakeSpeaker()
    let t = 0
    const speaker = new SpeakerClient(async (args) => {
      if (args[0] === 'agent' && args[1] === 'msg') {
        fake.sendResult = { event_id: 'ab'.repeat(32), published_to: 0, queued_for_retry: true }
      }
      return fake.runner(args)
    }, { identity: 'agent24' })
    const liveness = new InboundLiveness({
      speaker,
      identity: 'agent24',
      resolveSelfNpub: () => speaker.npubFor('agent24'),
      canaryIntervalMs: CANARY_MS,
      staleAfterMs: STALE_MS,
      now: () => t,
      rand: () => 0.5,
      log: { log: () => {}, warn: () => {}, error: () => {} },
    })

    await liveness.probe()
    expect(liveness.snapshot().last_error).toContain('outbox')
    // The next probe must be pushed OUT (backoff), never pulled in to base/3.
    expect(nextDelayOf(liveness)).toBeGreaterThan(CANARY_MS)
  })

  it('发布成功但没回来 → 才加速重探', async () => {
    const h = harness({ loseCanaries: true })
    await h.boot()
    await h.probe()
    expect(nextDelayOf(h.liveness)).toBeLessThan(CANARY_MS)
  })

  it('只剩没发出去的那条时不加速 —— 退避分支挡不住这一格', async () => {
    // The discriminating case for tracking `published` at all. A send that never
    // reached a relay sets the failure counter, so the backoff branch normally
    // answers first and hides the distinction. Here the counter has been reset
    // by a LATER successful send which then got confirmed and removed, leaving
    // only the queued canary — accelerating for it would mean probing hard at
    // something that has not yet had its chance to come back.
    const fake = new FakeSpeaker()
    let t = 0
    let publish = false
    let seq = 0
    const inFlight: { id: string; content: string }[] = []
    const speaker = new SpeakerClient(async (args) => {
      if (args[0] === 'agent' && args[1] === 'msg') {
        // A distinct id per SEND, not per delivered canary: deriving it from
        // inFlight.length gave the unpublished A and the published B the same
        // id, so B silently replaced A and the case under test never existed.
        const id = (++seq).toString(16).padStart(64, '0')
        const ci = args.indexOf('--content')
        if (publish) inFlight.push({ id, content: args[ci + 1]! })
        fake.sendResult = { event_id: id, published_to: publish ? 1 : 0, queued_for_retry: !publish }
      }
      return fake.runner(args)
    }, { identity: 'agent24' })
    const liveness = new InboundLiveness({
      speaker,
      identity: 'agent24',
      resolveSelfNpub: () => speaker.npubFor('agent24'),
      canaryIntervalMs: CANARY_MS,
      staleAfterMs: STALE_MS,
      now: () => t,
      rand: () => 0.5,
      log: { log: () => {}, warn: () => {}, error: () => {} },
    })
    await liveness.start()
    liveness.stop()

    await liveness.probe() // A: queued, never published
    publish = true
    await liveness.probe() // B: published
    for (const c of inFlight) {
      fake.inboxRows.push({
        id: c.id,
        sender_npub: SELF,
        plaintext: c.content,
        created_at: 1,
        is_incoming: true,
      })
    }
    await liveness.beat() // B confirms and leaves; only A remains, unpublished
    expect(liveness.snapshot().canaries.confirmed).toBe(1)
    expect(nextDelayOf(liveness)).toBeGreaterThanOrEqual(CANARY_MS)
  })

  it('outstanding 有硬上限,不会无界增长', async () => {
    const h = harness({ loseCanaries: true })
    await h.boot()
    for (let i = 0; i < 40; i++) await h.probe()
    expect(h.liveness.snapshot().canaries.outstanding).toBeLessThanOrEqual(12)
  })
})

describe('InboundLiveness — 健康文件是运维输入,必须当成不可信输入(M-2/M-3)', () => {
  it('损坏的健康文件不能让探针永远停在 starting', async () => {
    // `Math.max(0, NaN)` is NaN, and NaN > threshold is false for every
    // threshold — one bad field would make degraded unreachable forever.
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-liveness-'))
    const file = path.join(dir, 'health.json')
    try {
      for (const bad of [
        '{"seconds_since_confirmed":"broken","updated_at":"2026-09-02T00:00:00.000Z"}',
        '{"seconds_since_confirmed":120}',
        '{"seconds_since_confirmed":-5,"updated_at":"nonsense"}',
        'not json at all{',
        // Finite but absurd: 1e308 ms survives a bare isFinite check and then
        // overflows to Infinity on `* 1000`, which JSON.stringify writes back as
        // `null` — corrupting the NEXT restore too.
        '{"seconds_since_confirmed":1e308,"updated_at":"2026-09-02T00:00:00.000Z"}',
        // Counters as an array / with a bad field: silently coercing these to 0
        // breaks the very "counts accumulate across restarts" property the soak
        // criterion leans on, so the whole snapshot must be rejected loudly.
        '{"seconds_since_confirmed":1,"updated_at":"2026-09-02T00:00:00.000Z","canaries":[]}',
        '{"seconds_since_confirmed":1,"updated_at":"2026-09-02T00:00:00.000Z","canaries":{"sent":1,"confirmed":"x","lost":0}}',
        // Present-but-unparseable timestamp is corruption, not absence.
        '{"seconds_since_confirmed":1,"updated_at":"2026-09-02T00:00:00.000Z","last_confirmed_at":"soon"}',
      ]) {
        fs.writeFileSync(file, bad)
        const h = harness({ healthFile: file })
        await h.boot()
        h.advance(STALE_MS + 1)
        await h.judge()
        expect(h.liveness.current).toBe('degraded')
        expect(Number.isFinite(h.liveness.snapshot().seconds_since_confirmed)).toBe(true)
        expect(h.logs.some((l) => l.level === 'warn' && l.text.includes('健康快照'))).toBe(true)
      }
    } finally {
      fs.rmSync(dir, { recursive: true, force: true })
    }
  })

  it('读不了快照(不是「不存在」)要报警,不能当成首次运行', async () => {
    // Only ENOENT is a genuinely fresh start. EACCES / EISDIR / an I/O error
    // means a snapshot may well exist and we simply cannot see it — treating
    // that as "first run" hands a restart loop a clean slate every time, which
    // is exactly the accounting `restore()` exists to protect.
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-liveness-'))
    const file = path.join(dir, 'health.json')
    fs.mkdirSync(file) // a directory where the snapshot should be → EISDIR
    try {
      const h = harness({ healthFile: file })
      await h.boot()
      expect(h.logs.some((l) => l.level === 'warn' && l.text.includes('健康快照'))).toBe(true)
    } finally {
      fs.rmSync(dir, { recursive: true, force: true })
    }
  })

  it('计数跨重启累计 —— 泡测判据要求 confirmed 全程增长', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-liveness-'))
    const file = path.join(dir, 'health.json')
    try {
      const first = harness({ healthFile: file })
      await first.boot()
      await first.cycle()
      first.deliver()
      first.advance(1_000)
      await first.judge()
      expect(first.liveness.snapshot().canaries.confirmed).toBe(1)

      const second = harness({ healthFile: file, wallStart: 1_700_000_000_000 + 5_000 })
      await second.boot()
      await second.cycle()
      second.deliver()
      second.advance(1_000)
      await second.judge()
      // 2, not 1: a launchd restart must not read as "nothing ever came through".
      expect(second.liveness.snapshot().canaries.confirmed).toBe(2)
    } finally {
      fs.rmSync(dir, { recursive: true, force: true })
    }
  })
})

describe('配置的时间值必须是有限正数(L-3)', () => {
  it('负数/非数/Infinity 都回落到默认值,不会变成零延迟紧循环', () => {
    expect(durationMs('-1', 30_000, 1_000)).toBe(30_000)
    expect(durationMs('0', 30_000, 1_000)).toBe(30_000)
    expect(durationMs('abc', 30_000, 1_000)).toBe(30_000)
    expect(durationMs('Infinity', 30_000, 1_000)).toBe(30_000)
    expect(durationMs(undefined, 30_000, 1_000)).toBe(30_000)
    expect(durationMs('5', 30_000, 1_000)).toBe(1_000) // clamped to the floor
    expect(durationMs('1e30', 30_000, 1_000)).toBe(2 ** 31 - 1) // not 1ms
    expect(durationMs('45000', 30_000, 1_000)).toBe(45_000)
  })
})

describe('SpeakerClient — 探针依赖的两个 CLI 面', () => {
  it('npubFor 从 identity list 里按昵称取 npub', async () => {
    const fake = new FakeSpeaker()
    fake.identities = [
      { nickname: 'other', npub: 'npub1other' },
      { nickname: 'agent24', npub: SELF },
    ]
    const speaker = new SpeakerClient(fake.runner, { identity: 'agent24' })
    await expect(speaker.npubFor('agent24')).resolves.toBe(SELF)
    await expect(speaker.npubFor('nobody')).rejects.toThrow(/找不到身份/)
  })

  it('inbox 显式传 --limit(agent-speaker 默认 20,窗口太窄会漏消息也会漏 canary)', async () => {
    const fake = new FakeSpeaker()
    const speaker = new SpeakerClient(fake.runner, { identity: 'agent24', inboxLimit: 100 })
    await speaker.inbox()
    const call = fake.calls.find((c) => c.args[0] === 'history')!
    expect(call.args).toContain('--limit')
    expect(call.args[call.args.indexOf('--limit') + 1]).toBe('100')
  })
})
