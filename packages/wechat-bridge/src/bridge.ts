// The glue: a WeChat user's messages become agent24d runs in a per-user session,
// and run output (or an approval request) comes back over WeChat.
//
// Approval-over-WeChat composes with H3 durable resume: when a run parks on a
// decision we send the summary and wait; the user's "y"/"n" resolves the
// approval, the daemon resumes the run, and we re-await it for the final answer.

import type { Agent24Client, RunResult } from './agent24.js'
import type { Sender } from './ilink/sender.js'
import { messageText, type WeixinMessage } from './ilink/types.js'

export interface SessionStore {
  load(): Map<string, string>
  save(map: Map<string, string>): void
}

interface Pending {
  approvalId: string
  runId: string
  summary: string
}

/** Map a WeChat reply to an approval decision, or null if it isn't one. */
export function parseDecision(text: string): 'approve' | 'deny' | null {
  const t = text.trim().toLowerCase()
  if (['y', 'yes', 'ok', '好', '批准', '同意', '可以', '通过'].includes(t)) return 'approve'
  if (['n', 'no', '不', '拒绝', '不行', '取消'].includes(t)) return 'deny'
  return null
}

export class Bridge {
  private readonly sessions: Map<string, string>
  // from_user_id -> FIFO queue of parked approvals. A queue (not a single slot)
  // because a detached followUp() run can park while another is already parked;
  // a single slot would overwrite and orphan the earlier approval.
  private readonly pending = new Map<string, Pending[]>()
  // Per-user serialization tail. Handling for one user runs strictly in order so
  // concurrent messages (double-taps, WeChat retries) can't race on the session
  // or parked-approval maps. Bounded by the allowlist, so it needs no eviction.
  private readonly queues = new Map<string, Promise<unknown>>()

  constructor(
    private readonly agent: Agent24Client,
    private readonly sender: Sender,
    private readonly store: SessionStore,
    private readonly allowedUids: ReadonlySet<string>,
  ) {
    this.sessions = store.load()
  }

  /** Entry point for an inbound WeChat message. Enforces authorization, then
   * serializes handling per user. */
  async handle(msg: WeixinMessage): Promise<void> {
    const user = msg.from_user_id
    // Authorization gate — fail-closed. An unlisted sender is dropped and never
    // answered (so the bot isn't disclosed to strangers); the full id is logged
    // so the operator can authorize themselves via A24_WECHAT_ALLOWED_UIDS.
    if (!this.allowedUids.has(user)) {
      console.warn(
        `[wechat] 忽略未授权用户 ${user} 的消息；如需授权，将其加入 A24_WECHAT_ALLOWED_UIDS`,
      )
      return
    }
    await this.enqueue(user, () => this.process(msg))
  }

  /** Chain `fn` onto this user's serialization tail and return its result. */
  private enqueue<T>(user: string, fn: () => Promise<T>): Promise<T> {
    // `.then(fn, fn)` runs regardless of the prior task's outcome; the stored
    // tail is made non-throwing so one failure can't break the chain.
    const run = (this.queues.get(user) ?? Promise.resolve()).then(fn, fn)
    this.queues.set(
      user,
      run.catch(() => {}),
    )
    return run
  }

  private async process(msg: WeixinMessage): Promise<void> {
    const user = msg.from_user_id
    const ctx = msg.context_token
    // Any daemon call can throw (network / HTTP); without this the user would get
    // no reply at all for that message (only a server-side log). Always answer.
    try {
      await this.route(msg, user, ctx)
    } catch (err) {
      console.error('[wechat] 处理消息出错:', err instanceof Error ? err.message : err)
      await this.reply(user, ctx, `处理出错，请稍后重试：${err instanceof Error ? err.message : '未知错误'}`)
    }
  }

  private async route(msg: WeixinMessage, user: string, ctx: string): Promise<void> {
    const text = messageText(msg)
    if (!text) {
      await this.reply(user, ctx, '（我暂时只处理文字消息）')
      return
    }

    // 1. If this user has parked approvals, a y/n reply resolves the oldest.
    const queue = this.pending.get(user)
    if (queue && queue.length > 0) {
      const decision = parseDecision(text)
      if (!decision) {
        await this.reply(
          user,
          ctx,
          `你还有 ${queue.length} 条待批准的操作，先回复 y 批准 / n 拒绝最早的一条：\n${queue[0]!.summary}`,
        )
        return
      }
      const parked = queue.shift()! // FIFO — resolve in the order they arrived
      if (queue.length === 0) this.pending.delete(user)
      const ok = await this.agent.decide(parked.approvalId, decision)
      await this.reply(
        user,
        ctx,
        !ok
          ? '这条审批已失效或已被处理。'
          : decision === 'approve'
            ? '已批准，继续执行…'
            : '已拒绝。',
      )
      if (ok && decision === 'approve') {
        // The daemon resumes the run (H3); wait for the continued outcome.
        // Suppress deliver()'s own approval prompt here — the FIFO "surface next"
        // below is the single source of truth, so if the resumed run re-parks we
        // don't emit a second, potentially misleading prompt (a raw deliver()
        // prompt names the just-parked step, but FIFO resolves the OLDEST).
        await this.deliver(user, ctx, await this.agent.awaitRun(parked.runId), false)
      }
      // If more are queued (including one the resumed run may have just parked),
      // surface the next so the user knows what a following y/n applies to.
      const rest = this.pending.get(user)
      if (rest && rest.length > 0) {
        await this.reply(user, ctx, `还有 ${rest.length} 条待批准，下一条：\n${rest[0]!.summary}\n\n回复 y 批准 / n 拒绝`)
      }
      return
    }

    // 2. Otherwise, a new message runs in this user's session.
    await this.reply(user, ctx, '收到，开始执行…')
    const session = await this.sessionFor(user)
    await this.deliver(user, ctx, await this.agent.runToCompletion(text, session))
  }

  /** `surfaceApproval` false means the caller will prompt for the next approval
   * itself (the FIFO-resolve path) — deliver() just queues it silently. */
  private async deliver(
    user: string,
    ctx: string,
    result: RunResult,
    surfaceApproval = true,
  ): Promise<void> {
    switch (result.status) {
      case 'completed':
        await this.reply(user, ctx, result.text?.trim() || '（完成，无文本输出）')
        return
      case 'failed':
        await this.reply(user, ctx, `执行失败：${result.error ?? '未知错误'}`)
        return
      case 'cancelled':
        await this.reply(user, ctx, '已取消。')
        return
      case 'running':
        await this.reply(user, ctx, '还在处理中，完成后我再告诉你。')
        // Keep polling in the background (detached from this user's queue so new
        // messages aren't blocked); the final terminal result is re-enqueued so
        // its pending/session writes stay serialized with everything else.
        void this.followUp(user, ctx, result.runId)
        return
      case 'awaiting_approval': {
        const approval = (await this.agent.pendingApprovals()).find((a) => a.run_id === result.runId)
        if (approval) {
          const q = this.pending.get(user) ?? []
          q.push({ approvalId: approval.id, runId: result.runId, summary: approval.summary })
          this.pending.set(user, q)
          if (surfaceApproval) {
            await this.reply(user, ctx, `需要你批准：\n${approval.summary}\n\n回复 y 批准 / n 拒绝`)
          }
        } else {
          await this.reply(user, ctx, '有一步需要批准，请到桌面端处理。')
        }
        return
      }
      default:
        return
    }
  }

  /** For a run that hadn't finished within the bounded wait, keep polling in the
   * background and deliver the terminal result when it lands. Bounded so a run
   * that never finishes doesn't poll forever. */
  private async followUp(user: string, ctx: string, runId: string): Promise<void> {
    const MAX_ROUNDS = 6 // each awaitRun waits RUN_WAIT_TIMEOUT_MS — caps total wall-clock
    try {
      for (let i = 0; i < MAX_ROUNDS; i++) {
        const result = await this.agent.awaitRun(runId)
        if (result.status !== 'running') {
          // Re-enter the serialized queue so the delivery (and any pending-queue
          // write) stays ordered with the user's other messages.
          await this.enqueue(user, () => this.deliverSafely(user, ctx, result))
          return
        }
      }
      await this.reply(user, ctx, '这个任务还在后台运行，完成后请到桌面端查看结果。')
    } catch (err) {
      console.error('[wechat] 后台轮询出错:', err instanceof Error ? err.message : err)
      await this.reply(user, ctx, `后台任务出错：${err instanceof Error ? err.message : '未知错误'}`)
    }
  }

  /** deliver() guarded so a throw becomes a user-visible reply instead of a
   * silently dropped background result. */
  private async deliverSafely(user: string, ctx: string, result: RunResult): Promise<void> {
    try {
      await this.deliver(user, ctx, result)
    } catch (err) {
      console.error('[wechat] 投递结果出错:', err instanceof Error ? err.message : err)
      await this.reply(user, ctx, `处理出错，请稍后重试：${err instanceof Error ? err.message : '未知错误'}`)
    }
  }

  private async sessionFor(user: string): Promise<string> {
    let s = this.sessions.get(user)
    if (!s) {
      s = await this.agent.createSession(`WeChat ${user.slice(0, 8)}`)
      this.sessions.set(user, s)
      this.store.save(this.sessions)
    }
    return s
  }

  private async reply(user: string, ctx: string, text: string): Promise<void> {
    try {
      await this.sender.send(user, ctx, text)
    } catch (err) {
      console.error('[wechat] 发送失败:', err instanceof Error ? err.message : err)
    }
  }
}
