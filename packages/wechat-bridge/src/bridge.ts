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
  private readonly pending = new Map<string, Pending>() // from_user_id -> parked approval
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
    const text = messageText(msg)
    if (!text) {
      await this.reply(user, ctx, '（我暂时只处理文字消息）')
      return
    }

    // 1. If this user has a parked approval, a y/n reply resolves it.
    const parked = this.pending.get(user)
    if (parked) {
      const decision = parseDecision(text)
      if (decision) {
        this.pending.delete(user)
        const ok = await this.agent.decide(parked.approvalId, decision)
        if (!ok) {
          await this.reply(user, ctx, '这条审批已失效或已被处理。')
          return
        }
        await this.reply(user, ctx, decision === 'approve' ? '已批准，继续执行…' : '已拒绝。')
        if (decision === 'approve') {
          // The daemon resumes the run (H3); wait for the continued outcome.
          await this.deliver(user, ctx, await this.agent.awaitRun(parked.runId))
        }
        return
      }
      await this.reply(user, ctx, '你还有一条待批准的操作，先回复 y 批准 / n 拒绝。')
      return
    }

    // 2. Otherwise, a new message runs in this user's session.
    await this.reply(user, ctx, '收到，开始执行…')
    const session = await this.sessionFor(user)
    await this.deliver(user, ctx, await this.agent.runToCompletion(text, session))
  }

  private async deliver(user: string, ctx: string, result: RunResult): Promise<void> {
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
          this.pending.set(user, { approvalId: approval.id, runId: result.runId })
          await this.reply(user, ctx, `需要你批准：\n${approval.summary}\n\n回复 y 批准 / n 拒绝`)
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
    for (let i = 0; i < MAX_ROUNDS; i++) {
      const result = await this.agent.awaitRun(runId)
      if (result.status !== 'running') {
        await this.enqueue(user, () => this.deliver(user, ctx, result))
        return
      }
    }
    await this.reply(user, ctx, '这个任务还在后台运行，完成后请到桌面端查看结果。')
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
