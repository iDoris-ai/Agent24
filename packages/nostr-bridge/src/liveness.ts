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
// So we manufacture the signal instead, and it turns out the store already
// discriminates for us for free:
//
//   1. the bridge sends a canary message TO ITS OWN npub through the same
//      `agent-speaker` binary every other outbound message uses (G3 — the bridge
//      never opens a relay socket of its own);
//   2. the send stores an OUTGOING row keyed by the event id, `is_incoming=0`
//      (agent.go:237 → StoreOutgoingMessage);
//   3. `speaker.inbox()` already drops `is_incoming === false` rows, so that
//      row is invisible to us;
//   4. when — and only when — the DAEMON pulls that same event back off the
//      relay, `StoreIncomingMessage` REPLACEs the row on its primary key
//      (`INSERT OR REPLACE`, message.go:34) and `is_incoming` flips to 1;
//   5. the row appears in `inbox()`, carrying the event id we are holding.
//
// Seeing our own canary come back therefore proves the WHOLE inbound path is
// alive end to end: relay reachable, daemon running, daemon subscribing, daemon
// decrypting, daemon writing, bridge reading. That is a POSITIVE signal, which
// is the thing FU-32 says a timeout can never be.
//
// ── WHAT DELIBERATELY DOES *NOT* COUNT AS PROOF ─────────────────────────────
// Only a canary THIS PROCESS SENT and is still holding an outstanding id for can
// confirm, and each id confirms exactly once. Not: a canary recognised by its
// text (an old one sitting in the inbox window would then re-prove liveness on
// every poll, forever — a permanent fail-OPEN); not: an inbound peer message
// (rows repeat across polls, so "a message is in the window" says nothing about
// NOW). Fail-closed both ways: if the signal is ambiguous, it is not proof.

import { randomUUID } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { SpeakerTimeoutError, type InboundMessage, type SpeakerClient } from './speaker.js'

/** Marks a canary in the message body. NOT used for matching (see the header) —
 * it exists so an operator reading their own inbox knows what these are. */
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
  /** Drives the canary send — the same binary, the same relay flag as every
   * other outbound call. */
  speaker: SpeakerClient
  /** The agent-speaker identity nickname to send as. */
  identity: string
  /** Resolves this bridge's own npub. Async and retried because a keystore can
   * be briefly unreadable at boot; until it resolves we stay `starting` and
   * eventually go `degraded` with the reason, rather than silently not probing. */
  resolveSelfNpub: () => Promise<string>
  /** How often to emit a canary. */
  canaryIntervalMs: number
  /** No confirmation for this long ⇒ degraded. */
  staleAfterMs: number
  /** NIP-44-encrypt the canary (default true) so the probe exercises the exact
   * path peer traffic takes. Correctness does not depend on it: matching is by
   * event id, so even a canary the daemon failed to decrypt still confirms. */
  encrypt?: boolean
  /** Health snapshot path; empty disables the file. */
  healthFile?: string
  /** Static context recorded in the health file, for the operator. */
  context?: Record<string, unknown>
  /** Monotonic clock. `performance.now()` by default — NOT `Date.now()`: an NTP
   * step backwards would make the staleness check negative and silence the alarm
   * for hours, in exactly the unattended run it exists for (the same reasoning
   * as the log-gap cap in `main.ts`). */
  now?: () => number
  /** Wall clock, for human-readable timestamps in the health file only. */
  wallNow?: () => number
  log?: Pick<Console, 'log' | 'warn' | 'error'>
}

/** What the health file carries. Written for humans and `jq`, not for us. */
export interface LivenessSnapshot {
  state: LivenessState
  updated_at: string
  /** null until the first canary returns. */
  last_confirmed_at: string | null
  seconds_since_confirmed: number
  canaries: { sent: number; confirmed: number; lost: number; outstanding: number }
  last_error: string | null
  context?: Record<string, unknown>
}

/** Re-log a persistent degradation at most this often (it is already logged on
 * the transition itself). */
const DEGRADED_RELOG_MS = 60 * 60 * 1000
/** Rewrite an unchanged health file at most this often. */
const HEALTH_WRITE_MS = 60 * 1000

export class InboundLiveness {
  private readonly o: Required<
    Pick<LivenessOptions, 'canaryIntervalMs' | 'staleAfterMs' | 'encrypt' | 'now' | 'wallNow' | 'log'>
  > &
    LivenessOptions

  private selfNpub?: string
  private readonly startedAt: number
  private lastConfirmedAt: number | null = null
  private lastCanaryAt = -Infinity
  /** event id → monotonic send time. Bounded by staleAfterMs/canaryIntervalMs. */
  private readonly outstanding = new Map<string, number>()
  private state: LivenessState = 'starting'
  /** Silence between the previous confirmation and the one that just landed —
   * captured in `observe()` BEFORE `lastConfirmedAt` moves. The recovery message
   * needs it: computing the outage from `lastConfirmedAt` afterwards prints
   * "中断约 0s" for every outage however long, and computing it from when we
   * entered `degraded` under-reports it by the whole staleness window. What an
   * operator wants is how long messages were actually not getting through. */
  private lastGapMs = 0
  private sent = 0
  private confirmed = 0
  private lost = 0
  private lastError: string | null = null
  private lastDegradedLogAt = -Infinity
  private lastHealthWriteAt = -Infinity
  private lastHealthState?: LivenessState

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
      encrypt: opts.encrypt ?? true,
      now: opts.now ?? (() => performance.now()),
      wallNow: opts.wallNow ?? (() => Date.now()),
      log,
    }
    this.startedAt = this.o.now()
  }

  /** Current state — for tests and for whoever wants to gate on it. */
  get current(): LivenessState {
    return this.state
  }

  /** Called with every inbox read. Confirms liveness from any canary we are
   * holding, and returns the messages that are real peer traffic.
   *
   * Every row sent by US is dropped, not just the ones still outstanding: a
   * canary from an earlier run (or one that returned after we gave up on it)
   * would otherwise be handed to the inbound handler as if a peer had sent it. */
  observe(msgs: InboundMessage[]): InboundMessage[] {
    const peers: InboundMessage[] = []
    for (const m of msgs) {
      if (m.event_id && this.outstanding.delete(m.event_id)) {
        const now = this.o.now()
        this.confirmed += 1
        this.lastGapMs = now - (this.lastConfirmedAt ?? this.startedAt)
        this.lastConfirmedAt = now
        continue
      }
      // Dropped, never confirmed. Two independent recognisers because
      // `selfNpub` is only resolved on the first canary send, so on the very
      // first poll of a process it is still unset — and that is exactly the poll
      // that sees the canaries the PREVIOUS process left in the window. Matching
      // the marker text is safe HERE and nowhere else: the worst a peer can do
      // by putting the marker in their own message is get it ignored.
      if (this.selfNpub && m.from === this.selfNpub) continue
      if (m.content.startsWith(CANARY_PREFIX)) continue
      peers.push(m)
    }
    return peers
  }

  /** One beat of the probe: expire, judge, log, send, persist. Called once per
   * poll tick — INCLUDING ticks where the poll itself threw, because an outage
   * that makes every poll throw is exactly when the operator needs the verdict. */
  async beat(): Promise<void> {
    const now = this.o.now()
    this.expire(now)
    this.judge(now)
    if (now - this.lastCanaryAt >= this.o.canaryIntervalMs) await this.sendCanary()
    this.persist(now)
  }

  /** A canary that has been out longer than the staleness window is never coming
   * back; stop holding it (and stop it from confirming liveness long after the
   * fact, which would make a recovered path look continuously healthy). */
  private expire(now: number): void {
    for (const [id, at] of this.outstanding) {
      if (now - at > this.o.staleAfterMs) {
        this.outstanding.delete(id)
        this.lost += 1
      }
    }
  }

  private judge(now: number): void {
    // Before the first confirmation the clock runs from process start: a bridge
    // whose canary NEVER returns (daemon not started, wrong relay) must go
    // degraded on its own, not sit in `starting` forever.
    const since = this.lastConfirmedAt ?? this.startedAt
    const elapsed = now - since
    const next: LivenessState =
      elapsed > this.o.staleAfterMs ? 'degraded' : this.lastConfirmedAt === null ? 'starting' : 'ok'
    const was = this.state
    this.state = next

    if (next === 'degraded') {
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

  private async sendCanary(): Promise<void> {
    // Stamped BEFORE the await: a send that hangs to its 60s deadline must not
    // let the next beat fire another one on top of it.
    this.lastCanaryAt = this.o.now()
    let npub: string
    try {
      npub = this.selfNpub ?? (this.selfNpub = await this.o.resolveSelfNpub())
    } catch (err) {
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
        this.lastError = 'canary 已发出但 agent-speaker 未返回 event_id,无法追踪往返'
        return
      }
      // Tracked even when `published_to` is 0: a queued canary that the daemon's
      // outbox publishes later still round-trips, and that is still proof.
      this.outstanding.set(res.event_id, this.o.now())
      this.lastError =
        (res.published_to ?? 0) === 0 ? 'canary 未直达任何 relay(已入 outbox 重试队列)' : null
    } catch (err) {
      // A killed child is INDETERMINATE, not a failure — the canary may well be
      // on a relay. Either way we hold no id for it, so it can never confirm.
      this.lastError =
        err instanceof SpeakerTimeoutError
          ? `canary 发送超时被杀,投递状态未知:${err.message}`
          : `canary 发送失败:${err instanceof Error ? err.message : String(err)}`
    }
  }

  snapshot(now = this.o.now()): LivenessSnapshot {
    const since = this.lastConfirmedAt ?? this.startedAt
    const wall = this.o.wallNow()
    return {
      state: this.state,
      updated_at: new Date(wall).toISOString(),
      last_confirmed_at:
        this.lastConfirmedAt === null
          ? null
          : new Date(wall - (now - this.lastConfirmedAt)).toISOString(),
      seconds_since_confirmed: Math.round((now - since) / 1000),
      canaries: {
        sent: this.sent,
        confirmed: this.confirmed,
        lost: this.lost,
        outstanding: this.outstanding.size,
      },
      last_error: this.lastError,
      ...(this.o.context ? { context: this.o.context } : {}),
    }
  }

  /** Write the health snapshot. On state change immediately; otherwise at most
   * once a minute — a 7-day soak at a 5s poll would be ~120k writes otherwise. */
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
