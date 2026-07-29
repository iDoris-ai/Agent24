// F4a outbound: the agent24 side of the Nostr channel. Exposes the transport
// envelopes as verbs, drives agent-speaker underneath, and default-registers the
// agent's business capabilities. Inbound (listen → gated runs) is F4b.

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { randomUUID } from 'node:crypto'
import { makeContent, type ContentInput } from './protocol.js'
import { SpeakerClient, type DiscoverEntry, type PublishResult, type SendResult } from './speaker.js'
import {
  toAgentSpeakerProfile,
  type Agent24Profile,
  type AgentSpeakerProfile,
} from './profile.js'

export class NostrBridge {
  constructor(
    private readonly speaker: SpeakerClient,
    /** The agent-speaker identity (nickname) this bridge acts as. */
    private readonly identity: string,
  ) {}

  /** register (default on first run): publish this agent's BUSINESS capabilities
   * to the network so others can discover it by capability. Writes the
   * agent-speaker AgentProfile to a temp JSON and hands it to `profile publish
   * --json-file` (agent-speaker eats JSON, not YAML). */
  async register(
    name: string,
    profile: Agent24Profile,
    now: number = Date.now(),
  ): Promise<{ profile: AgentSpeakerProfile; result: PublishResult }> {
    const asp = toAgentSpeakerProfile(name, profile, Math.floor(now / 1000))
    const file = path.join(os.tmpdir(), `agent24-profile-${randomUUID()}.json`)
    fs.writeFileSync(file, JSON.stringify(asp), { mode: 0o600 })
    let result: PublishResult
    try {
      result = await this.speaker.publishProfile(this.identity, file)
    } finally {
      fs.rmSync(file, { force: true })
    }
    // A profile that reached no relays isn't discoverable — surface it.
    if ((result.published_to ?? 0) === 0) {
      throw new Error('register reached no relays (profile not published)')
    }
    return { profile: asp, result }
  }

  /** say / answer: a directed 1:1 message carrying an intent envelope. Pass
   * `replyTo`/`threadId` inside `content` to continue a thread. */
  async say(toNpub: string, content: ContentInput, now?: number): Promise<SendResult> {
    const envelope = makeContent(content, now)
    const res = await this.speaker.sendMessage(this.identity, toNpub, JSON.stringify(envelope))
    // A message that reached zero relays and wasn't even queued for retry never
    // went out — surface that as a failure instead of resolving as success.
    // (queued_for_retry is a legitimate degraded state: agent-speaker's outbox
    // will keep trying, so it is NOT an error.)
    if ((res.published_to ?? 0) === 0 && !res.queued_for_retry) {
      throw new Error(`say to ${toNpub} reached no relays (published_to=0, not queued)`)
    }
    return res
  }

  /** search: locate agents by business capability. */
  async search(capability: string): Promise<DiscoverEntry[]> {
    return this.speaker.discover(capability)
  }
}
