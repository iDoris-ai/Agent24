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
// canary. Hence `jitter` below — it is not decoration, it is what keeps a
// correlated failure from becoming a permanent false alarm — plus a faster
// re-probe while anything is outstanding, so one lost canary cannot walk us to
// the staleness threshold. The real fix is upstream (store before publish, or
// make `is_incoming` monotonic on upsert).

import { randomUUID } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { SpeakerTimeoutError, type InboundMessage, type SpeakerClient } from './speaker.js'

/** Marks a canary in the message body. NOT used for matching — it exists so an
 * operator reading their own inbox knows what these are. */
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
  canaries: { sent: number; confirmed: number; lost: number; outstanding: number }
  last_error: string | null
  context?: Record<string, unknown>
}

/** Re-log a persistent degradation at most this often (it is already logged on
 * the transition itself). */
const DEGRADED_RELOG_MS = 60 * 60 * 1000
/** Rewrite an unchanged health file at most this often. */
const HEALTH_WRITE_MS = 60 * 1000

interface Outstanding {
  at: number
  /** Exactly what we sent, so the round trip proves decryption too. */
  content: string
}

export class InboundLiveness {
  private readonly o: Required<
    Pick<
      LivenessOptions,
      'canaryIntervalMs' | 'staleAfterMs' | 'tickMs' | 'encrypt' | 'now' | 'wallNow' | 'rand' | 'log'
    >
  > &
    LivenessOptions

  private selfNpub?: string
  private readonly startedAt: number
  private lastConfirmedAt: number | null = null
  private lastConfirmedWall: number | null = null
  /** Silence inherited from the previous process (see `restore`). Without it a
   * supervisor that restarts the bridge every few minutes would hand each new
   * process a fresh grace period and `degraded` could never be reached. */
  private restoredGapMs = 0
  private lastCanaryAt = -Infinity
  private nextCanaryDelay: number
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
  private lastDegradedLogAt = -Infinity
  private lastHealthWriteAt = -Infinity
  private lastHealthState?: LivenessState
  private timer?: ReturnType<typeof setTimeout>
  private stopped = false
  private beating = false

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
    this.nextCanaryDelay = this.jittered()
  }

  /** Current state — for tests and for whoever wants to gate on it. */
  get current(): LivenessState {
    return this.state
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
    this.restore()
    try {
      this.selfNpub = await this.o.resolveSelfNpub()
    } catch (err) {
      // Not fatal and not silent: recorded, retried on every canary send, and it
      // walks us to `degraded` on its own if it never succeeds.
      this.lastError = `无法解析本机 npub(${err instanceof Error ? err.message : String(err)})`
      this.o.log.warn(`[nostr] ⚠️  活性探针:${this.lastError};将重试。`)
    }
    this.schedule()
  }

  stop(): void {
    this.stopped = true
    if (this.timer) clearTimeout(this.timer)
  }

  private schedule(): void {
    if (this.stopped) return
    this.timer = setTimeout(() => void this.loop(), this.o.tickMs)
  }

  private async loop(): Promise<void> {
    if (this.stopped) return
    // Re-entrancy guard: a canary send can sit on its 60s deadline, which is
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
    this.schedule()
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
        continue
      }
      // Dropped, never confirmed: a canary from an earlier run, or one that came
      // back after we gave up on it, must not be handed to the inbound handler
      // as if a peer had sent it. Matched on SENDER, not on the marker text — a
      // text match would also swallow a legitimate peer message that happened to
      // start with the marker.
      if (this.selfNpub && m.from === this.selfNpub) continue
      peers.push(m)
    }
    return peers
  }

  /** One beat: read the inbox, confirm, judge, log, probe, persist. */
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
    if (now - this.lastCanaryAt >= this.dueIn()) await this.sendCanary()
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

  /** While a canary is outstanding, probe again sooner: one canary lost to the
   * upstream store race must not be able to walk us to the staleness threshold
   * on its own. */
  private dueIn(): number {
    return this.outstanding.size > 0
      ? Math.max(1_000, Math.round(this.o.canaryIntervalMs / 3))
      : this.nextCanaryDelay
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
    this.nextCanaryDelay = this.jittered()
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
      this.outstanding.set(res.event_id, { at: this.o.now(), content })
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

  /** Carry the previous process's silence forward. Without this, a bridge that a
   * supervisor restarts every few minutes gets a fresh grace period every time
   * and can NEVER reach `degraded` — the soak would report a clean run while the
   * inbound path had been dead the whole week. Wall clock, because a monotonic
   * one means nothing across processes; `max(0, …)` so a backwards clock step
   * shortens the inherited gap rather than making it negative. */
  private restore(): void {
    const file = this.o.healthFile
    if (!file) return
    try {
      const prev = JSON.parse(fs.readFileSync(file, 'utf8')) as Partial<LivenessSnapshot>
      const writtenAt = prev.updated_at ? Date.parse(prev.updated_at) : NaN
      const sinceWrite = Number.isFinite(writtenAt) ? Math.max(0, this.o.wallNow() - writtenAt) : 0
      const carried = Math.max(0, (prev.seconds_since_confirmed ?? 0) * 1000)
      this.restoredGapMs = carried + sinceWrite
      const confirmedAt = prev.last_confirmed_at ? Date.parse(prev.last_confirmed_at) : NaN
      if (Number.isFinite(confirmedAt)) this.lastConfirmedWall = confirmedAt
      if (this.restoredGapMs > this.o.staleAfterMs) {
        this.o.log.warn(
          `[nostr] ⚠️  上一轮进程留下的入站静默已有 ${Math.round(this.restoredGapMs / 1000)}s,本进程不重新计时(重启不清账)。`,
        )
      }
    } catch {
      /* no previous run, or an unreadable file — start fresh */
    }
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
