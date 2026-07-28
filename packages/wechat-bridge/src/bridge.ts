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

  constructor(
    private readonly agent: Agent24Client,
    private readonly sender: Sender,
    private readonly store: SessionStore,
  ) {
    this.sessions = store.load()
  }

  /** Entry point for an inbound WeChat message. */
  async handle(msg: WeixinMessage): Promise<void> {
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
