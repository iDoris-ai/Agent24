// F4b inbound: an agent message from the network becomes a GATED agent24d run
// in a per-sender session, and the run's output goes back over Nostr as an
// `answer`. Fail-closed on an npub allowlist (every inbound message would
// otherwise run as an owner-level agent24d run — the F3 lesson).

import { Agent24Client, type RunResult } from './agent24.js'
import { SpeakerClient, type InboundMessage } from './speaker.js'
import { makeContent, PROTOCOL_VERSION, type Content } from './protocol.js'

/** Turn an inbound message into a run prompt. If it carries an F4 envelope, tag
 * the prompt with the intent and pull a human-readable body out of the payload;
 * otherwise the raw content is the prompt. Returns the parsed envelope (if any)
 * so the reply can thread off it. */
export function envelopeToPrompt(content: string): { prompt: string; envelope?: Content } {
  try {
    const env = JSON.parse(content) as Content
    if (env && env.version === PROTOCOL_VERSION && typeof env.intent === 'string') {
      const p = (env.payload ?? {}) as Record<string, unknown>
      const body = p.text ?? p.q ?? p.question ?? p.message ?? p.prompt
      const text = typeof body === 'string' ? body : JSON.stringify(env.payload ?? {})
      return { prompt: `[from a peer agent · intent:${env.intent}] ${text}`, envelope: env }
    }
  } catch {
    /* not an F4 envelope — treat as raw text */
  }
  return { prompt: content }
}

/** Cap on the dedup set so a 7×24 soak can't grow it without bound. */
const MAX_SEEN = 10_000

export class InboundBridge {
  private readonly sessions = new Map<string, string>() // npub -> session id
  private readonly seen = new Set<string>() // event_id dedup (bounded, insertion-ordered)
  // Per-sender serialization so concurrent messages from one npub can't race on
  // the session map. Bounded by the allowlist, so it needs no eviction.
  private readonly queues = new Map<string, Promise<unknown>>()

  constructor(
    private readonly agent: Agent24Client,
    private readonly speaker: SpeakerClient,
    /** This bridge's own agent-speaker identity, for replies. */
    private readonly identity: string,
    /** Fail-closed allowlist of sender npubs allowed to drive a run. */
    private readonly allowedNpubs: ReadonlySet<string>,
  ) {}

  /** Entry point for one inbound message. Authorizes, dedups, then serializes
   * per sender. */
  async handle(msg: InboundMessage): Promise<void> {
    if (!this.allowedNpubs.has(msg.from)) {
      console.warn(`[nostr] 忽略未授权 agent ${msg.from} 的消息;如需授权加入 A24_NOSTR_ALLOWED_NPUBS`)
      return
    }
    const eventId = msg.event_id
    if (eventId) {
      if (this.seen.has(eventId)) return // dedup (also guards concurrent polls)
      this.rememberSeen(eventId) // mark BEFORE running so a concurrent poll dedups
    }
    await this.enqueue(msg.from, () => this.process(msg)).catch((err) => {
      // A run that reached a terminal state (even a failed one) replies and does
      // NOT throw — it stays deduped. A THROW here is a transient failure
      // (daemon down / network), so un-commit the event and let a later inbox
      // poll retry it, rather than silently losing the message.
      if (eventId) this.seen.delete(eventId)
      console.error('[nostr] 处理入站消息出错(将在下次轮询重试):', err instanceof Error ? err.message : err)
    })
  }

  /** Record an event id, evicting the oldest once over the cap (Set preserves
   * insertion order). */
  private rememberSeen(id: string): void {
    this.seen.add(id)
    if (this.seen.size > MAX_SEEN) {
      const excess = this.seen.size - MAX_SEEN
      let i = 0
      for (const oldest of this.seen) {
        if (i++ >= excess) break
        this.seen.delete(oldest)
      }
    }
  }

  private enqueue<T>(key: string, fn: () => Promise<T>): Promise<T> {
    const run = (this.queues.get(key) ?? Promise.resolve()).then(fn, fn)
    this.queues.set(
      key,
      run.catch(() => {}),
    )
    return run
  }

  private async process(msg: InboundMessage): Promise<void> {
    // Throws (daemon down / network) propagate to handle()'s catch, which
    // un-commits the event for retry. A terminal run replies here and never
    // throws, so it stays deduped.
    const { prompt, envelope } = envelopeToPrompt(msg.content)
    const session = await this.sessionFor(msg.from)
    const result = await this.agent.runToCompletion(prompt, session)
    await this.reply(msg.from, result, envelope, msg.event_id)
  }

  private async sessionFor(npub: string): Promise<string> {
    let s = this.sessions.get(npub)
    if (!s) {
      s = await this.agent.createSession(`Nostr ${npub.slice(0, 12)}`)
      this.sessions.set(npub, s)
    }
    return s
  }

  private async reply(
    toNpub: string,
    result: RunResult,
    inbound: Content | undefined,
    replyToEvent: string | undefined,
  ): Promise<void> {
    const text =
      result.status === 'completed'
        ? result.text?.trim() || '(完成,无文本输出)'
        : result.status === 'awaiting_approval'
          ? '这一步需要我的主人批准,稍候由 ta 在桌面端处理后我再回复。'
          : result.status === 'failed'
            ? `执行失败:${result.error ?? '未知错误'}`
            : result.status === 'cancelled'
              ? '这次请求已被取消。'
              : '还在处理中,完成后我再告诉你。'
    // Reply as an `answer`, threaded off the inbound envelope so multi-round
    // collaboration correlates.
    const envelope = makeContent({
      intent: 'answer',
      threadId: inbound?.thread_id,
      replyTo: replyToEvent,
      topic: inbound?.topic,
      payload: { text },
    })
    try {
      await this.speaker.sendMessage(this.identity, toNpub, JSON.stringify(envelope))
    } catch (err) {
      console.error('[nostr] 回复失败:', err instanceof Error ? err.message : err)
    }
  }
}

/** Poll the inbox once and feed each message to the bridge. A caller loops this
 * on an interval (the `listen` verb); errors on one poll don't kill the loop. */
export async function pollOnce(speaker: SpeakerClient, bridge: InboundBridge): Promise<void> {
  const msgs = await speaker.inbox()
  for (const msg of msgs) {
    if (msg.from) await bridge.handle(msg)
  }
}
