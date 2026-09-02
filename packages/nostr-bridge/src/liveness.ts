// FU-32 — inbound liveness. The bridge must be able to tell "收件箱为空" from
// "入站通路已死". Today it cannot: `pollOnce` reads a LOCAL SQLite store that the
// agent-speaker daemon fills, so when the daemon's relay path breaks (machine
// woke with no network, relay unreachable, daemon not running at all) the CLI
// still exits 0 with `[]`. Nothing throws, no timeout fires, the failure counter
// in `main.ts` counts exceptions and there are none. The bridge looks healthy
// and silently answers nobody — for days.
//
// ── WHY A CANARY AND NOT A "last-seen" READ ─────────────────────────────────
// The obvious fix is to read the daemon's own liveness state. It does not have
// one: verified against agent-speaker/hyphae @98834db — `internal/daemon/
// daemon.go` exposes no status command, writes no heartbeat, and `watchOneRelay`
// SWALLOWS the dial error (`if err != nil { return 0 }`, line 309-312) so a
// relay it cannot reach is indistinguishable, even in its own stdout, from a
// relay with nothing new. There is no last-seen to read. Adding one is an
// upstream change in another repo (see FU-33).
//
// So we manufacture the signal instead, and the store already discriminates for
// us for free:
//
//   1. the bridge sends a canary message TO ITS OWN npub through the same
//      `agent-speaker` binary every other outbound message uses (G3 — the bridge
//      never opens a relay socket of its own);
//   2. the send stores an OUTGOING row keyed by the event id, `is_incoming=0`
//      (agent.go:238 → StoreOutgoingMessage);
//   3. `speaker.inbox()` already drops `is_incoming === false` rows, so that
//      row is invisible to us;
//   4. when — and only when — the DAEMON pulls that same event back off the
//      relay, `StoreIncomingMessage` REPLACEs the row on its primary key
//      (`INSERT OR REPLACE`, message.go:34) and `is_incoming` flips to 1;
//   5. the row appears in `inbox()`, carrying the event id we are holding AND
//      the plaintext we sent.
//
// Steps 2/4/5 are not inferred from reading that code — they were VERIFIED by
// running it: a probe against the real upstream schema showed the outgoing row
// landing with `is_incoming=0`, then the same event id REPLACING it (one row,
// not two) with `is_incoming=1`, id equal to the 64-hex `event_id` that `agent
// msg --json` returns. NIP-44 to one's own key round-trips too (checked against
// `pkg/crypto`), so an encrypted canary is fine. FU-33 proposes contributing
// that probe upstream so a refactor there cannot silently invalidate this.
//
// Seeing our own canary come back therefore proves the inbound path end to end:
// relay reachable, daemon running, daemon subscribing, daemon DECRYPTING (we
// compare the plaintext, not just the id — the daemon stores undecryptable
// messages too, so an id-only match would call a broken crypto path healthy),
// daemon writing, bridge reading. That is a POSITIVE signal, which is the thing
// FU-32 says a timeout can never be.
//
// ── WHAT DELIBERATELY DOES *NOT* COUNT AS PROOF ─────────────────────────────
// Only a canary THIS PROCESS SENT, still outstanding, arriving from our own
// npub with the exact plaintext we sent, can confirm — and each id confirms
// exactly once. Not: a canary recognised by its marker text (an old one sitting
// in the inbox window would then re-prove liveness on every poll, forever — a
// permanent fail-OPEN); not: an inbound peer message (rows repeat across polls,
// so "a message is in the window" says nothing about NOW). If the signal is
// ambiguous, it is not proof.
//
// ── THE KNOWN HAZARD THIS CANNOT FIX FROM HERE (FU-33) ──────────────────────
// Upstream publishes to the relay BEFORE it stores the outgoing row
// (agent.go:208-238). If the daemon happens to be inside its 3s subscribe window
// it can receive the event and write `is_incoming=1` first, and then the sending
// CLI's `INSERT OR REPLACE` puts the row back to 0 — while the daemon's `seen`
// set now holds that id and will never process it again. The canary is then lost
// on a perfectly healthy system. The reason this is not merely an occasional
// dropped probe: a fixed 5-minute canary against the daemon's 30s cycle is an
// exact multiple, so the two PHASE-LOCK, and an unlucky phase loses EVERY
// canary — a permanent false alarm on a system that is fine.
//
// Two things defend against that, and BOTH are needed:
//   - `jittered()` draws a continuous ±20% delay, and
//   - the canary runs on ITS OWN timer (`armCanary`), not on the judge tick.
// The second is not a refactor. A jittered deadline that is only CHECKED on a
// fixed 30s tick still fires at `t0 + N×30s`, so `mod 30s` — the only thing that
// decides whether the send lands inside the daemon's subscribe window — never
// moves, and the random draw merely picks how many ticks to wait. The jitter has
// to reach the actual timer to do anything at all.
// Plus a faster re-probe while a PUBLISHED canary is outstanding, so a single
// lost canary cannot walk us to the staleness threshold (see `nextDelay`).
// The real fix is upstream (store before publish, or make `is_incoming`
// monotonic on upsert).

import { randomUUID } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { SpeakerTimeoutError, type InboundMessage, type SpeakerClient } from './speaker.js'

/** Marks a canary in the message body. NEVER used for matching — neither to
 * confirm nor to drop. It exists so an operator reading their own inbox knows
 * what these are. Matching on it would let any peer opt out of being processed
 * by prefixing their message with a public string. */
export const CANARY_PREFIX = 'a24-liveness-canary'

export type LivenessState =
  /** No canary has come back yet, and we have not been waiting long enough to
   * call it broken. The state a healthy bridge is in for its first minutes. */
  | 'starting'
  /** A canary came back within the staleness window. */
  | 'ok'
  /** Nothing has come back for longer than the staleness window. Inbound is
   * presumed dead — this is the state the whole module exists to reach. */
  | 'degraded'

export interface LivenessOptions {
  /** Drives both the canary send and the probe's OWN inbox read — the same
   * binary, the same relay flag as every other outbound call. */
  speaker: SpeakerClient
  /** The agent-speaker identity nickname to send as. */
  identity: string
  /** Resolves this bridge's own npub. Async and retried because a keystore can
   * be briefly unreadable at boot; until it resolves we stay `starting` and
   * eventually go `degraded` with the reason, rather than silently not probing. */
  resolveSelfNpub: () => Promise<string>
  /** How often to emit a canary (jittered ±20%, see the header). */
  canaryIntervalMs: number
  /** No confirmation for this long ⇒ degraded. */
  staleAfterMs: number
  /** How often the probe wakes up. Independent of the poll loop on purpose. */
  tickMs?: number
  /** NIP-44-encrypt the canary (default true) so the probe exercises the exact
   * path peer traffic takes — and so that comparing the returned plaintext
   * actually tests the daemon's decryption. */
  encrypt?: boolean
  /** Health snapshot path; empty disables the file (and cross-restart memory). */
  healthFile?: string
  /** Static context recorded in the health file, for the operator. */
  context?: Record<string, unknown>
  /** Monotonic clock. `performance.now()` by default — NOT `Date.now()`: an NTP
   * step backwards would make the staleness check negative and silence the alarm
   * for hours, in exactly the unattended run it exists for (the same reasoning
   * as the log-gap cap in `main.ts`). */
  now?: () => number
  /** Wall clock. Used ONLY for timestamps and for carrying the silence across a
   * restart — never for the staleness decision itself. */
  wallNow?: () => number
  /** [0,1) source for the jitter. Injectable so tests are deterministic. */
  rand?: () => number
  log?: Pick<Console, 'log' | 'warn' | 'error'>
}

/** What the health file carries. Written for humans and `jq` — and read back by
 * the next process, so the silence survives a restart (see `restore`). */
export interface LivenessSnapshot {
  state: LivenessState
  updated_at: string
  /** null until a canary has ever come back. */
  last_confirmed_at: string | null
  seconds_since_confirmed: number
  /** How many times the path has gone from healthy to dead, CUMULATIVE across
   * restarts. The file only ever holds the CURRENT state, so a degradation that
   * happened on Tuesday and recovered by Wednesday leaves no trace in it — and
   * the F5 criterion is "never degraded across seven days". This counter is that
   * evidence; without it the criterion cannot be checked from what the run
   * leaves behind, only from having watched it live. */
  degraded_transitions: number
  canaries: { sent: number; confirmed: number; lost: number; outstanding: number }
  last_error: string | null
  /** Our own npub, so a restart whose keystore read fails can still recognise
   * (and drop) the canaries the previous process left in the inbox window. */
  self_npub?: string
  context?: Record<string, unknown>
}

/** Re-log a persistent degradation at most this often (it is already logged on
 * the transition itself). */
const DEGRADED_RELOG_MS = 60 * 60 * 1000
/** Rewrite an unchanged health file at most this often. */
const HEALTH_WRITE_MS = 60 * 1000

/** Final clamp before any `setTimeout`. Validating the CONFIG value is not
 * enough: a canary interval sitting at the 2^31-1 ceiling still gets multiplied
 * by up to 1.2 by the jitter, and Node silently turns an out-of-range delay into
 * 1ms — the tight loop the config validation existed to prevent, reached by
 * arithmetic downstream of it. Clamp where the timer is actually armed. */
function safeDelay(ms: number): number {
  return Number.isFinite(ms) ? Math.min(Math.max(0, ms), 2 ** 31 - 1) : 0
}

interface Outstanding {
  at: number
  /** Exactly what we sent, so the round trip proves decryption too. */
  content: string
  /** Whether it actually reached a relay. A canary sitting in agent-speaker's
   * outbox has not been given a chance to come back yet, so it must not drive
   * the faster re-probe — otherwise a real network outage turns into a
   * self-inflicted flood of queued messages. */
  published: boolean
}

/** Hard cap on tracked canaries, so no schedule bug can grow it without bound. */
const MAX_OUTSTANDING = 12

export class InboundLiveness {
  private readonly o: Required<
    Pick<
      LivenessOptions,
      'canaryIntervalMs' | 'staleAfterMs' | 'tickMs' | 'encrypt' | 'now' | 'wallNow' | 'rand' | 'log'
    >
  > &
    LivenessOptions

  private selfNpub?: string
  private startedAt: number
  private lastConfirmedAt: number | null = null
  private lastConfirmedWall: number | null = null
  /** Silence inherited from the previous process (see `restore`). Without it a
   * supervisor that restarts the bridge every few minutes would hand each new
   * process a fresh grace period and `degraded` could never be reached. */
  private restoredGapMs = 0
  private consecutiveSendFailures = 0
  private readonly outstanding = new Map<string, Outstanding>()
  private state: LivenessState = 'starting'
  /** Silence between the previous confirmation and the one that just landed —
   * captured BEFORE `lastConfirmedAt` moves, because the recovery message has to
   * report how long messages were actually not getting through. */
  private lastGapMs = 0
  private sent = 0
  private confirmed = 0
  private lost = 0
  private lastError: string | null = null
  private degradedTransitions = 0
  private lastDegradedLogAt = -Infinity
  private lastHealthWriteAt = -Infinity
  private lastHealthState?: LivenessState
  /** Judge/persist on a fixed cadence. */
  private tickTimer?: ReturnType<typeof setTimeout>
  /** Canary sends on their OWN continuously-jittered deadline — see `armCanary`. */
  private canaryTimer?: ReturnType<typeof setTimeout>
  private started = false
  private stopped = false
  private beating = false
  private sending = false

  constructor(opts: LivenessOptions) {
    const log = opts.log ?? console
    let stale = opts.staleAfterMs
    // A staleness window shorter than two canary intervals guarantees a false
    // alarm between beats. Clamp, but say so — silently "fixing" the operator's
    // config is how a knob ends up meaning nothing.
    const floor = opts.canaryIntervalMs * 2 + 60_000
    if (stale < floor) {
      log.warn(
        `[nostr] ⚠️  活性陈旧阈值 ${stale}ms 小于两个 canary 周期,必然误报;已抬到 ${floor}ms。`,
      )
      stale = floor
    }
    this.o = {
      ...opts,
      canaryIntervalMs: opts.canaryIntervalMs,
      staleAfterMs: stale,
      tickMs: opts.tickMs ?? Math.min(30_000, opts.canaryIntervalMs),
      encrypt: opts.encrypt ?? true,
      now: opts.now ?? (() => performance.now()),
      wallNow: opts.wallNow ?? (() => Date.now()),
      rand: opts.rand ?? Math.random,
      log,
    }
    this.startedAt = this.o.now()
  }

  /** Re-anchored in `start()`: `restore()` measures the inherited silence up to
   * ITS OWN wall-clock moment, and `elapsed()` then adds `now - startedAt` on
   * top. If the two anchors differ — an instance constructed and started minutes
   * apart — that interval is counted twice. */
  private reanchor(): void {
    this.startedAt = this.o.now()
  }

  /** Current state — for tests and for whoever wants to gate on it. */
  get current(): LivenessState {
    return this.state
  }

  /** Whether our own npub is known. Until it is, `observe()` cannot recognise
   * our own leftover canaries, so inbound dispatch has to hold (see `pollOnce`).
   * Resolved from the keystore at `start()`, or from the last trusted health
   * snapshot when that read fails. */
  get ready(): boolean {
    return this.selfNpub !== undefined
  }

  /** Resolve our own npub and pick up where the previous process left off, then
   * start the probe's own timer.
   *
   * The probe runs on its OWN clock, NOT inside the poll loop, and does its own
   * inbox read. That is load-bearing: `pollOnce` awaits every inbound message's
   * agent run to completion, and a run can take many minutes, so a probe riding
   * the poll loop would be starved exactly when a backlog is being worked — the
   * health file would sit at a stale `ok` for hours during a real outage, and a
   * healthy bridge working two slow messages would cross the staleness threshold
   * and cry wolf. */
  async start(): Promise<void> {
    // Idempotent: a second call would otherwise leave two self-rescheduling
    // chains running forever, since only the latest handle is kept.
    if (this.started) return
    this.started = true
    this.restore()
    this.reanchor()
    try {
      this.selfNpub = await this.o.resolveSelfNpub()
    } catch (err) {
      // Not fatal and not silent: recorded, retried on every canary send, and it
      // walks us to `degraded` on its own if it never succeeds.
      this.lastError = `无法解析本机 npub(${err instanceof Error ? err.message : String(err)})`
      this.o.log.warn(`[nostr] ⚠️  活性探针:${this.lastError};将重试。`)
    }
    this.scheduleTick()
    // The first canary goes out almost immediately (so a healthy bridge reaches
    // `ok` in seconds rather than minutes) but at a RANDOM offset, because the
    // phase of the very first send seeds every send after it.
    this.armCanary(Math.round(this.o.rand() * 2_000))
  }

  stop(): void {
    this.stopped = true
    if (this.tickTimer) clearTimeout(this.tickTimer)
    if (this.canaryTimer) clearTimeout(this.canaryTimer)
  }

  private scheduleTick(): void {
    if (this.stopped) return
    this.tickTimer = setTimeout(() => void this.loop(), safeDelay(this.o.tickMs))
  }

  private async loop(): Promise<void> {
    if (this.stopped) return
    // Re-entrancy guard: an inbox read can sit on its 60s deadline, which is
    // longer than a tick.
    if (!this.beating) {
      this.beating = true
      try {
        await this.beat()
      } catch (err) {
        this.o.log.error('[nostr] 活性探针出错:', err instanceof Error ? err.message : err)
      } finally {
        this.beating = false
      }
    }
    this.scheduleTick()
  }

  /** Arm the next canary on its own timer.
   *
   * This is separate from the judge tick, and that separation is the entire
   * defence against the phase lock described in the header. Checking a jittered
   * DEADLINE on a fixed 30s tick does not help: the send still lands on
   * `t0 + N×30s`, so `mod 30s` — the thing that decides whether we sit inside
   * the daemon's 3s subscribe window — never moves, and the random draw only
   * picks how many ticks to wait. Only an independent timer with a continuous
   * delay makes the send phase actually drift. */
  private armCanary(delay: number): void {
    if (this.stopped) return
    if (this.canaryTimer) clearTimeout(this.canaryTimer)
    this.canaryTimer = setTimeout(() => void this.canaryTurn(), safeDelay(delay))
  }

  private async canaryTurn(): Promise<void> {
    if (this.stopped) return
    if (!this.sending) {
      this.sending = true
      try {
        await this.probe()
      } catch (err) {
        this.o.log.error('[nostr] 活性探针发送出错:', err instanceof Error ? err.message : err)
      } finally {
        this.sending = false
      }
    }
    this.armCanary(this.nextDelay())
  }

  /** Called with every inbox read — the probe's own, and `pollOnce`'s. Confirms
   * liveness from a canary we are holding, and returns the messages that are
   * real peer traffic. */
  observe(msgs: InboundMessage[]): InboundMessage[] {
    const peers: InboundMessage[] = []
    for (const m of msgs) {
      const pending = m.event_id ? this.outstanding.get(m.event_id) : undefined
      if (pending) {
        this.outstanding.delete(m.event_id!)
        // The comparison is byte-exact on purpose, and that is safe: the full
        // upstream pipeline the canary rides — encrypt → zstd+base64 compress →
        // relay → decompress → decrypt — was verified to round-trip byte for
        // byte (there is no size threshold that would skip compression on one
        // leg only). If it were lossy, this check would false-alarm forever on a
        // healthy system, so it is not something to assume.
        if (m.content !== pending.content) {
          // It came back, but not as what we sent. The daemon stores messages it
          // could not decrypt (daemon.go:358-368), so this is the shape of a
          // broken crypto path — under which real peer messages reach the agent
          // as ciphertext. Emphatically not a confirmation.
          this.lost += 1
          this.lastError =
            'canary 原路返回但内容不是我们发出的明文 —— daemon 的解密链路可能已坏(对端消息会以密文进来)'
          continue
        }
        const now = this.o.now()
        this.confirmed += 1
        this.lastGapMs = this.elapsed(now)
        this.lastConfirmedAt = now
        this.lastConfirmedWall = this.o.wallNow()
        // A completed round trip is the STRONGEST evidence there is, so it must
        // clear the send-failure backoff — otherwise this is an absorbing state:
        // a canary that missed the relay on the first try (failures=1) but got
        // published later by agent-speaker's outbox comes back and confirms,
        // while the schedule stays backed off to 20-30 minutes. Two of those and
        // the next probe lands past the 15-minute staleness window, so a path
        // that is demonstrably working reports `degraded` on a cycle.
        if (this.consecutiveSendFailures > 0) {
          this.consecutiveSendFailures = 0
          this.armCanary(this.nextDelay()) // pull the deadline back in
        }
        continue
      }
      // Dropped, never confirmed: a canary from an earlier run, or one that came
      // back after we gave up on it, must not be handed to the inbound handler
      // as if a peer had sent it. Matched on SENDER, because a text match would
      // also swallow a legitimate peer message that happened to start with the
      // marker.
      // Matched on SENDER only. Falling back to the marker TEXT when the npub is
      // not yet known would swallow a legitimate peer message that happens to
      // start with the marker — a public string anyone can type. The npub comes
      // from the keystore, or, when that read fails at boot, from the last
      // trusted health snapshot (see `restore`), which covers the window that
      // fallback was there for without giving a peer a way to silence itself.
      if (this.selfNpub && m.from === this.selfNpub) continue
      peers.push(m)
    }
    return peers
  }

  /** One beat: read the inbox, confirm, judge, log, persist. Sending is NOT done
   * here — it has its own timer, so a canary send hanging on its 60s deadline
   * cannot delay the verdict or the health file that publishes it. */
  async beat(): Promise<void> {
    try {
      this.observe(await this.o.speaker.inbox())
    } catch (err) {
      // A failed read is not evidence of anything either way — but it must not
      // skip the judgement below, which is the whole point of this method.
      this.lastError = `读收件箱失败:${err instanceof Error ? err.message : String(err)}`
    }
    const now = this.o.now()
    this.expire(now)
    this.judge(now)
    this.persist(now)
  }

  /** Silence so far: since this process's last confirmation, or — before any —
   * carried over from the previous process plus this process's own uptime. */
  private elapsed(now: number): number {
    return this.lastConfirmedAt !== null
      ? now - this.lastConfirmedAt
      : this.restoredGapMs + (now - this.startedAt)
  }

  /** ±20% so the canary cadence cannot phase-lock with the daemon's fixed 30s
   * watch cycle (see the header — that lock is what turns an occasional lost
   * probe into a permanent false alarm). */
  private jittered(): number {
    return Math.round(this.o.canaryIntervalMs * (0.8 + this.o.rand() * 0.4))
  }

  /** Delay until the next canary.
   *
   * Three regimes, in priority order:
   *   1. sends are FAILING (threw, or reached no relay) — exponential backoff.
   *      Without it a real network outage becomes self-inflicted damage: every
   *      unpublished canary lands in agent-speaker's outbox and is retried from
   *      there, so probing hard through a week-long outage would queue thousands
   *      of messages to fix nothing. The cap stays under the staleness window's
   *      order of magnitude so recovery is still noticed promptly.
   *   2. a PUBLISHED canary is outstanding — probe sooner, so a single canary
   *      lost to the upstream store race cannot walk us to the threshold on its
   *      own. Only published ones count: one still sitting in the outbox has not
   *      had its chance to come back yet, and treating it as "missing" is what
   *      would turn regime 1 into a flood.
   *   3. otherwise — the jittered base interval. */
  private nextDelay(): number {
    if (this.consecutiveSendFailures > 0) {
      const factor = Math.min(2 ** this.consecutiveSendFailures, 8)
      return Math.min(this.o.canaryIntervalMs * factor, 30 * 60_000)
    }
    let published = 0
    for (const c of this.outstanding.values()) if (c.published) published++
    if (published > 0) return Math.max(1_000, Math.round(this.o.canaryIntervalMs / 3))
    return this.jittered()
  }

  /** A canary out longer than the staleness window is never coming back; stop
   * holding it, so it cannot confirm long after the fact and make a recovered
   * path look continuously healthy. */
  private expire(now: number): void {
    for (const [id, c] of this.outstanding) {
      if (now - c.at > this.o.staleAfterMs) {
        this.outstanding.delete(id)
        this.lost += 1
      }
    }
  }

  private judge(now: number): void {
    const elapsed = this.elapsed(now)
    const next: LivenessState =
      elapsed > this.o.staleAfterMs ? 'degraded' : this.lastConfirmedAt === null ? 'starting' : 'ok'
    const was = this.state
    this.state = next

    if (next === 'degraded') {
      if (was !== 'degraded') this.degradedTransitions += 1
      if (was !== 'degraded' || now - this.lastDegradedLogAt >= DEGRADED_RELOG_MS) {
        this.lastDegradedLogAt = now
        this.o.log.error(
          [
            `[nostr] ❌ 入站通路疑似已死:${Math.round(elapsed / 1000)}s 没有任何入站确认(阈值 ${Math.round(this.o.staleAfterMs / 1000)}s)。`,
            `        canary 已发 ${this.sent} / 确认 ${this.confirmed} / 丢失 ${this.lost}${this.lastError ? ` · 最近错误:${this.lastError}` : ''}`,
            '        进程还活着、日程照跑,但对端发来的消息现在收不到。按这个顺序查:',
            '        1) agent-speaker daemon 还在跑吗(它是入站的唯一来源,桥只读它写的本地库)',
            '        2) 桥的 --relay 和 daemon 的 --relay 是不是同一个(不一致就是发出去没人收)',
            '        3) 机器刚唤醒/换网的话,网络是否已恢复',
          ].join('\n'),
        )
      }
      return
    }
    if (was === 'degraded') {
      this.o.log.log(`[nostr] ✅ 入站通路已恢复(中断约 ${Math.round(this.lastGapMs / 1000)}s)。`)
      this.lastDegradedLogAt = -Infinity
    } else if (was === 'starting' && next === 'ok') {
      this.o.log.log('[nostr] ✅ 入站通路已确认(canary 往返成功)。')
    }
  }

  /** Send one canary. Public so tests can drive a probe without waiting on real
   * timers; production calls it from `canaryTurn`. */
  async probe(): Promise<void> {
    if (this.outstanding.size >= MAX_OUTSTANDING) {
      // Nothing is coming back and we are already degraded; stop adding to the
      // pile (and to agent-speaker's outbox) until expiry drains it.
      return
    }
    let npub: string
    try {
      npub = this.selfNpub ?? (this.selfNpub = await this.o.resolveSelfNpub())
    } catch (err) {
      this.consecutiveSendFailures += 1
      this.lastError = `无法解析本机 npub(${err instanceof Error ? err.message : String(err)})`
      return
    }
    const content = `${CANARY_PREFIX} ${randomUUID()}`
    try {
      const res = await this.o.speaker.sendMessage(this.o.identity, npub, content, this.o.encrypt)
      this.sent += 1
      if (!res.event_id) {
        // Untrackable: without the id we cannot recognise the round trip, and
        // matching on text would be the fail-open this module refuses.
        this.consecutiveSendFailures += 1
        this.lastError = 'canary 已发出但 agent-speaker 未返回 event_id,无法追踪往返'
        return
      }
      const published = (res.published_to ?? 0) > 0
      // Tracked even when unpublished: a queued canary that the daemon's outbox
      // publishes later still round-trips, and that is still proof. It just does
      // not earn the faster re-probe (see `nextDelay`).
      this.outstanding.set(res.event_id, { at: this.o.now(), content, published })
      this.consecutiveSendFailures = published ? 0 : this.consecutiveSendFailures + 1
      this.lastError = published ? null : 'canary 未直达任何 relay(已入 outbox 重试队列)'
    } catch (err) {
      // A killed child is INDETERMINATE, not a failure — the canary may well be
      // on a relay. Either way we hold no id for it, so it can never confirm.
      this.consecutiveSendFailures += 1
      this.lastError =
        err instanceof SpeakerTimeoutError
          ? `canary 发送超时被杀,投递状态未知:${err.message}`
          : `canary 发送失败:${err instanceof Error ? err.message : String(err)}`
    }
  }

  snapshot(now = this.o.now()): LivenessSnapshot {
    return {
      state: this.state,
      updated_at: new Date(this.o.wallNow()).toISOString(),
      // The stored wall timestamp, NOT one back-computed from the monotonic
      // clock: on macOS the monotonic clock does not advance across sleep, so
      // subtracting it from wall time would report a confirmation from before an
      // 8-hour sleep as "a few minutes ago".
      last_confirmed_at:
        this.lastConfirmedWall === null ? null : new Date(this.lastConfirmedWall).toISOString(),
      seconds_since_confirmed: Math.round(this.elapsed(now) / 1000),
      degraded_transitions: this.degradedTransitions,
      canaries: {
        sent: this.sent,
        confirmed: this.confirmed,
        lost: this.lost,
        outstanding: this.outstanding.size,
      },
      last_error: this.lastError,
      ...(this.selfNpub ? { self_npub: this.selfNpub } : {}),
      ...(this.o.context ? { context: this.o.context } : {}),
    }
  }

  /** Carry the previous process's silence forward. Without this, a bridge that a
   * supervisor restarts every few minutes gets a fresh grace period every time
   * and can NEVER reach `degraded` — the soak would report a clean run while the
   * inbound path had been dead the whole week. Wall clock, because a monotonic
   * one means nothing across processes; `max(0, …)` so a backwards clock step
   * shortens the inherited gap rather than making it negative. */
  private restore(): void {
    const file = this.o.healthFile
    if (!file) return
    let raw: string
    try {
      raw = fs.readFileSync(file, 'utf8')
    } catch (err) {
      // ONLY a missing file is a genuinely fresh start. EACCES / EISDIR / an I/O
      // error means a snapshot may well exist and we simply cannot see it —
      // treating that as "first run" hands a restart loop a clean slate every
      // time, which is precisely the accounting this function exists to protect.
      if ((err as NodeJS.ErrnoException)?.code !== 'ENOENT') this.unreadable(file, err)
      return
    }
    // Every value out of this file is validated. `Math.max(0, NaN)` is NaN, and a
    // NaN elapsed compares false against EVERY threshold — so one corrupt or
    // hand-edited field would leave the probe permanently in `starting`, unable
    // to ever report degraded. A fail-open hiding in an operations file.
    //
    // "Finite" is not sufficient either: 1e308 milliseconds is finite, survives
    // the check, and then overflows to Infinity the moment it is multiplied —
    // which JSON.stringify writes back out as `null`, corrupting the NEXT run's
    // restore too. Everything is therefore bounded to a safe integer range.
    const num = (v: unknown, max = Number.MAX_SAFE_INTEGER): number | null =>
      typeof v === 'number' && Number.isSafeInteger(v) && v >= 0 && v <= max ? v : null
    const stamp = (v: unknown): number | null => {
      if (typeof v !== 'string') return null
      const t = Date.parse(v)
      return Number.isFinite(t) ? t : null
    }
    try {
      const prev = JSON.parse(raw) as Partial<LivenessSnapshot>
      // A year of silence is already absurd; anything beyond it is corruption,
      // and bounding here keeps `* 1000` inside the safe integer range.
      const carriedSec = num(prev.seconds_since_confirmed, 365 * 24 * 3600)
      const writtenAt = stamp(prev.updated_at)
      if (carriedSec === null || writtenAt === null) {
        throw new Error('seconds_since_confirmed / updated_at 缺失或非法')
      }
      // `last_confirmed_at` may legitimately be absent (nothing has ever come
      // back), but a PRESENT-and-unparseable one is corruption, not absence.
      if (prev.last_confirmed_at != null && stamp(prev.last_confirmed_at) === null) {
        throw new Error('last_confirmed_at 存在但不是合法时间戳')
      }
      // Counters must be a plain object of non-negative safe integers. An array,
      // a null, or one bad field silently became 0 before — which quietly
      // violates the very "counts accumulate across restarts" property the soak
      // criterion leans on.
      const c = prev.canaries
      const plainObject =
        typeof c === 'object' && c !== null && !Array.isArray(c)
      const counters = plainObject
        ? {
            sent: num((c as LivenessSnapshot['canaries']).sent),
            confirmed: num((c as LivenessSnapshot['canaries']).confirmed),
            lost: num((c as LivenessSnapshot['canaries']).lost),
          }
        : null
      if (c !== undefined && (!counters || Object.values(counters).some((v) => v === null))) {
        throw new Error('canaries 计数缺失或非法')
      }
      const transitions = prev.degraded_transitions === undefined
        ? 0
        : num(prev.degraded_transitions)
      if (transitions === null) throw new Error('degraded_transitions 非法')
      // The previous process's VERDICT, not just its silence. It decides whether
      // this process's first judgement is a new transition:
      //   · it was already `degraded` → the transition is in the inherited count
      //     already, so counting again would inflate it on every restart;
      //   · it was `ok`/`starting` and we come up past the threshold → the
      //     transition genuinely happened between the two processes (it may have
      //     been killed before it could judge), and NOT counting it would let a
      //     real degradation vanish from the only evidence the soak keeps.
      const prevState = prev.state
      if (prevState !== undefined && !['starting', 'ok', 'degraded'].includes(prevState)) {
        throw new Error('state 非法')
      }

      // The written value is already the TOTAL silence (it includes whatever the
      // previous process itself inherited), so adding the downtime since the
      // write accumulates correctly across any number of restarts — it does not
      // double-count.
      this.restoredGapMs = carriedSec * 1000 + Math.max(0, this.o.wallNow() - writtenAt)
      this.lastConfirmedWall = stamp(prev.last_confirmed_at)
      this.sent = counters?.sent ?? 0
      this.confirmed = counters?.confirmed ?? 0
      this.lost = counters?.lost ?? 0
      this.degradedTransitions = transitions
      // Last known identity, so a keystore we cannot read right now does not
      // leave us unable to recognise our own leftover canaries.
      if (typeof prev.self_npub === 'string' && prev.self_npub) this.selfNpub = prev.self_npub
      if (prevState !== undefined) this.state = prevState
      if (this.restoredGapMs > this.o.staleAfterMs) {
        this.o.log.warn(
          `[nostr] ⚠️  上一轮进程留下的入站静默已有 ${Math.round(this.restoredGapMs / 1000)}s,本进程不重新计时(重启不清账)。`,
        )
      }
    } catch (err) {
      this.unreadable(file, err)
    }
  }

  /** The snapshot exists but cannot be trusted. Reset the inherited accounting
   * and say so loudly — a silent fresh start here is indistinguishable from a
   * healthy first run, which is how a broken soak reports PASS. */
  private unreadable(file: string, err: unknown): void {
    this.restoredGapMs = 0
    this.o.log.warn(
      `[nostr] ⚠️  健康快照 ${file} 存在但无法读取/解析(${err instanceof Error ? err.message : String(err)});静默计时从本进程重新开始 —— 若这是反复出现的,活性判据不可信。`,
    )
  }

  /** Write the health snapshot. On state change immediately; otherwise at most
   * once a minute — a 7-day soak would be a lot of writes otherwise. */
  private persist(now: number): void {
    const file = this.o.healthFile
    if (!file) return
    const changed = this.state !== this.lastHealthState
    if (!changed && now - this.lastHealthWriteAt < HEALTH_WRITE_MS) return
    this.lastHealthWriteAt = now
    this.lastHealthState = this.state
    const tmp = `${file}.tmp`
    try {
      fs.mkdirSync(path.dirname(file), { recursive: true })
      fs.writeFileSync(tmp, `${JSON.stringify(this.snapshot(now), null, 2)}\n`, { mode: 0o600 })
      // Atomic: a reader (or the operator's `cat`) never sees a half-written file.
      fs.renameSync(tmp, file)
    } catch (err) {
      // The health file is diagnostics; failing to write it must not take the
      // bridge down. Say it once per state change so it is not invisible either.
      if (changed) {
        this.o.log.warn(
          `[nostr] ⚠️  写健康快照失败 ${file}:`,
          err instanceof Error ? err.message : err,
        )
      }
      fs.rmSync(tmp, { force: true })
    }
  }
}
