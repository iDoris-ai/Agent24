// Thin wrapper over the agent-speaker CLI (G3: drive the binary, don't
// reimplement Nostr). Every call shells out to `agent-speaker <cmd> ... --json`
// and unwraps the result. The runner is injectable so tests drive a hermetic
// fake instead of a real subprocess (see testing/fake-speaker.ts).
//
// Verified against the real binary (7cef326 / agent-speaker#29) during 联调:
//   - all `--json` output is an ENVELOPE: {ok:true,data:<result>} on success,
//     {ok:false,error,message} on failure (+ a semantic non-zero exit).
//   - `agent msg` takes `--from`; `profile publish` / `agent inbox` take `--as`.
//   - the acting identity's keystore must be UNLOCKED — headless means a
//     no-password identity (an encrypted keystore can't be unlocked
//     non-interactively today; see the F4 doc / CC-82 R3).

import { execFile } from 'node:child_process'
import { createHash } from 'node:crypto'
import { CONFIG } from './config.js'
import type { AgentSpeakerProfile } from './profile.js'

/** Runs one agent-speaker invocation and resolves its stdout. */
export type SpeakerRunner = (args: string[]) => Promise<string>

/** The real runner: exec the `agent-speaker` binary. A `--json` error is itself
 * a JSON envelope (on stdout or stderr) plus a non-zero exit, so prefer the JSON
 * — `unwrap` surfaces the real message. Reject only when there's no JSON to
 * interpret (e.g. the binary is missing).
 *
 * The `timeout` is LOAD-BEARING, not defensive garnish: the caller (`tick()` in
 * `main.ts`) only schedules the next poll after this promise settles, so a child
 * that never calls back stops inbound permanently and silently. `killSignal`
 * escalates past a child ignoring SIGTERM. See `CONFIG.SPEAKER_TIMEOUT_MS`. */
export function cliRunner(bin = 'agent-speaker', timeoutMs = CONFIG.SPEAKER_TIMEOUT_MS): SpeakerRunner {
  return (args) =>
    new Promise<string>((resolve, reject) => {
      execFile(
        bin,
        args,
        { maxBuffer: 8 * 1024 * 1024, timeout: timeoutMs, killSignal: 'SIGKILL' },
        (err, stdout, stderr) => {
          const out = (stdout && stdout.trim()) || (stderr && stderr.trim()) || ''
          if (out.startsWith('{') || out.startsWith('[')) return resolve(out)
          // A killed child reports `err.killed`; say so explicitly, because
          // "failed: null" tells an operator nothing at 3am.
          const killed = (err as (Error & { killed?: boolean }) | null)?.killed
          if (killed) {
            return reject(
              new Error(`agent-speaker ${args[0] ?? ''} timed out after ${timeoutMs}ms and was killed`),
            )
          }
          if (err) return reject(new Error(`agent-speaker ${args[0] ?? ''} failed: ${stderr.trim() || err.message}`))
          resolve(stdout)
        },
      )
    })
}

/** `agent msg` result (envelope `data`). */
export interface SendResult {
  event_id?: string
  published_to?: number
  relay_count?: number
  relays?: { url: string; ok: boolean; error?: string }[]
  queued_for_retry?: boolean
}

/** `profile publish` result (envelope `data`, agent-speaker#29). */
export interface PublishResult {
  name?: string
  published_to?: number
  relay_count?: number
  relays?: { url: string; ok: boolean; error?: string }[]
}

/** One `profile discover` entry (envelope `data[]`). */
export interface DiscoverEntry {
  npub: string
  profile: AgentSpeakerProfile
}

/** A decrypted inbound message, normalized from agent-speaker's StoredMessage.
 * `content` is the raw string the sender put on the wire (an F4 envelope JSON
 * when it came from another agent24). */
export interface InboundMessage {
  /** Sender npub — the allowlist / session key. */
  from: string
  content: string
  event_id?: string
  created_at?: number
}

export interface SpeakerOptions {
  /** The acting identity nickname — required for inbox (`--as`). */
  identity?: string
  /** Relay override; falls back to agent-speaker's default when unset. */
  relay?: string
}

export class SpeakerClient {
  private readonly identity?: string
  private readonly relay?: string

  constructor(
    private readonly run: SpeakerRunner,
    opts: SpeakerOptions = {},
  ) {
    this.identity = opts.identity
    this.relay = opts.relay
  }

  private relayArgs(): string[] {
    return this.relay ? ['--relay', this.relay] : []
  }

  /** register → `profile publish --as <id> --json-file <file> --json`. */
  async publishProfile(identity: string, jsonFile: string): Promise<PublishResult> {
    const out = await this.run([
      'profile',
      'publish',
      '--as',
      identity,
      '--json-file',
      jsonFile,
      '--json',
      ...this.relayArgs(),
    ])
    return unwrap<PublishResult>(out, 'profile publish')
  }

  /** say / answer → `agent msg --from <id> --to <npub>` (NIP-44 encrypted). */
  async sendMessage(from: string, to: string, content: string, encrypt = true): Promise<SendResult> {
    const out = await this.run([
      'agent',
      'msg',
      '--from',
      from,
      '--to',
      to,
      '--content',
      content,
      `--encrypt=${encrypt}`,
      '--json',
      ...this.relayArgs(),
    ])
    return unwrap<SendResult>(out, 'agent msg')
  }

  /** search → `profile discover --capability <cap> --json` (returns data[]). */
  async discover(capability: string): Promise<DiscoverEntry[]> {
    const out = await this.run([
      'profile',
      'discover',
      '--capability',
      capability,
      '--json',
      ...this.relayArgs(),
    ])
    const data = unwrap<unknown>(out, 'profile discover')
    return Array.isArray(data) ? (data as DiscoverEntry[]) : []
  }

  /** listen/subscribe (inbound) → `agent inbox --as <id> --json`. The daemon
   * pulls inbound to the local store; this reads it.
   *
   * Verified in 联调: `agent inbox --json` returns `{time, from, content,
   * encrypted, decrypted}` — `from` is the sender npub, `content` is the
   * plaintext (`decrypted` is a boolean flag, NOT text), and there is NO id.
   * So we SYNTHESIZE a stable dedup key from sender+time+content — without it
   * every poll re-processes the same (last-N) window of messages. Fallbacks are
   * kept for a version skew (older StoredMessage-shaped output). */
  async inbox(): Promise<InboundMessage[]> {
    // history inbox (daemon-populated local store), NOT agent inbox: agent
    // inbox's `from` is a truncated/nickname DISPLAY string that never matches
    // the full-npub allowlist; history inbox's StoredMessage has the full
    // sender_npub. `--as <identity>` targets THIS agent's inbox directly
    // (agent-speaker#30) — no need to hijack the keystore default identity, so
    // multiple bridges can share a keystore without racing on `identity use`.
    const args = ['history', 'inbox', ...(this.identity ? ['--as', this.identity] : []), '--json']
    const data = unwrap<unknown>(await this.run(args), 'history inbox')
    const rows = Array.isArray(data) ? data : ((data as { messages?: unknown[] })?.messages ?? [])
    return (rows as Record<string, unknown>[])
      .filter((r) => r.is_incoming !== false) // absent → it's the inbox, all incoming
      .map((r) => {
        const from = String(r.sender_npub ?? r.from ?? r.from_npub ?? r.sender ?? '')
        const content = String(r.plaintext ?? r.content ?? r.text ?? '')
        // history inbox `id` is the real hex event id (agent-speaker#30 fixed the
        // raw-byte encoding), matching `agent msg`'s returned event_id byte for
        // byte — use it as the dedup key. The synthesized sender+created_at+content
        // key is retained only as a defensive fallback for any legacy pre-#30 row
        // whose id is still non-hex garbage (those age out of the store).
        const realId =
          typeof r.id === 'string' && /^[0-9a-f]{16,}$/i.test(r.id) ? r.id : undefined
        const stamp = String(r.created_at ?? r.received_at ?? r.time ?? '')
        const event_id =
          realId ??
          createHash('sha1').update(`${from} ${stamp} ${content}`).digest('hex').slice(0, 24)
        return {
          from,
          content,
          event_id,
          created_at: typeof r.created_at === 'number' ? r.created_at : undefined,
        }
      })
  }
}

interface Envelope {
  ok?: boolean
  data?: unknown
  error?: string
  message?: string
}

/** Unwrap agent-speaker's `--json` envelope: return `data` on `{ok:true}`, throw
 * on `{ok:false}`. Tolerates a bare (un-enveloped) payload from older builds. */
function unwrap<T>(raw: string, label: string): T {
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    throw new Error(`agent-speaker ${label}: expected JSON, got: ${raw.trim().slice(0, 200)}`)
  }
  if (parsed && typeof parsed === 'object' && 'ok' in (parsed as Envelope)) {
    const env = parsed as Envelope
    if (!env.ok) {
      throw new Error(
        `agent-speaker ${label} failed: ${env.message ?? env.error ?? 'unknown error'}`,
      )
    }
    return env.data as T
  }
  return parsed as T
}
