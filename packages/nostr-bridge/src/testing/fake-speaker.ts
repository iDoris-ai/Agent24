// Hermetic FakeNostr harness (the FakeSlack pattern, mirroring F3's FakeILink):
// a fake `agent-speaker` runner that the REAL SpeakerClient/NostrBridge drive.
// It records every invocation and returns canned JSON, so the whole outbound
// path — envelope construction, YAML→JSON profile transform, temp-file publish,
// arg building — is exercised end-to-end with no subprocess, network, or relay.
//
// FakeNostr lands here (with F4) rather than in H11 because there was no Nostr
// adapter to test until now — "先有消费者再有提供者".

import fs from 'node:fs'
import type { SpeakerRunner, InboundMessage } from '../speaker.js'
import type { AgentSpeakerProfile } from '../profile.js'

export interface Invocation {
  args: string[]
  /** For `profile publish --json-file`, the parsed content of that file. */
  publishedProfile?: AgentSpeakerProfile
}

export class FakeSpeaker {
  /** Every agent-speaker call the bridge made, in order. */
  readonly calls: Invocation[] = []
  /** Canned `agent msg --json` reply. */
  sendResult: unknown = {
    event_id: 'evt_fake',
    published_to: 1,
    relay_count: 1,
    relays: [{ url: 'wss://relay.aastar.io', ok: true }],
    queued_for_retry: false,
  }
  /** Canned `profile discover --json` reply. */
  discoverResult: unknown = []
  /** Canned `agent inbox --json` reply (inbound messages). */
  inboxMessages: InboundMessage[] = []

  /** The runner to hand SpeakerClient. */
  runner: SpeakerRunner = (args) => this.handle(args)

  private async handle(args: string[]): Promise<string> {
    const inv: Invocation = { args }
    const [group, cmd] = args
    if (group === 'profile' && cmd === 'publish') {
      const i = args.indexOf('--json-file')
      const file = i >= 0 ? args[i + 1] : undefined
      if (file) inv.publishedProfile = JSON.parse(fs.readFileSync(file, 'utf8')) as AgentSpeakerProfile
      this.calls.push(inv)
      return '' // this command has no --json output (CC-82 gap); exit 0 = success
    }
    if (group === 'agent' && cmd === 'msg') {
      this.calls.push(inv)
      return JSON.stringify(this.sendResult)
    }
    if (group === 'profile' && cmd === 'discover') {
      this.calls.push(inv)
      return JSON.stringify(this.discoverResult)
    }
    if (group === 'agent' && cmd === 'inbox') {
      this.calls.push(inv)
      return JSON.stringify(this.inboxMessages)
    }
    this.calls.push(inv)
    return '{}'
  }

  /** The value passed to `agent msg --content` in the last say(), parsed. */
  lastContent(): Record<string, unknown> | undefined {
    const msg = [...this.calls].reverse().find((c) => c.args[0] === 'agent' && c.args[1] === 'msg')
    if (!msg) return undefined
    const i = msg.args.indexOf('--content')
    return i >= 0 ? (JSON.parse(msg.args[i + 1]!) as Record<string, unknown>) : undefined
  }
}
