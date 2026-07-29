// F4 envelope + intent protocol (docs/specs/F4-nostr-channel.md §4).
//
// Three transport ENVELOPES (say / announce / listen) carry an open INTENT that
// the agent's own LLM generates — the protocol only guarantees the envelope and
// the correlation fields, never the collaboration workflow. This module is the
// content-JSON shape that rides inside a message.

import { randomUUID } from 'node:crypto'

/** The three transport envelopes. agent24 exposes exactly these; register /
 * discover fold into announce / listen against the directory topic. */
export type Envelope = 'say' | 'announce' | 'listen'

/** Recommended intent vocabulary. OPEN by design — the agent LLM may emit any
 * string; receivers match on the ones they understand and fall back to the
 * free-form payload + topic for the rest. */
export type Intent =
  | 'ask'
  | 'answer'
  | 'offer'
  | 'accept'
  | 'decline'
  | 'inform'
  | 'report'
  | 'cfp'
  | 'ack'
  | 'tip'
  | (string & {})

export const PROTOCOL_VERSION = 'f4/1' as const

/** The JSON carried in a message's `content` (§4.3). No `sender` field — the
 * Nostr event's signed pubkey IS the identity. */
export interface Content {
  version: typeof PROTOCOL_VERSION
  intent: Intent
  /** Correlation: multi-round negotiation / CFP threads hang off this. */
  thread_id: string
  /** The event this answers/accepts, when it is a reply. */
  reply_to?: string
  /** Routing / topic-vector matching. */
  topic?: string
  tags?: string[]
  /** Intent-specific free-form body the agent LLM fills. */
  payload?: Record<string, unknown>
  /** App-layer expiry (F4 self-judges; NOT relay-enforced — see §4.5). */
  expires_at?: number
  /** Async receipt for long-running collaboration. */
  status?: 'ok' | 'working' | 'failed'
  /** Structured error when status = failed. */
  error?: { code: string; message: string } | null
}

export interface ContentInput {
  intent: Intent
  /** Omit to start a new thread; pass an existing id to continue one. */
  threadId?: string
  replyTo?: string
  topic?: string
  tags?: string[]
  payload?: Record<string, unknown>
  /** Seconds-from-now this message stays relevant; becomes an absolute
   * `expires_at`. */
  ttlSeconds?: number
  status?: Content['status']
  error?: Content['error']
}

/** Build a well-formed content envelope, minting a thread id when none is given.
 * `now` is injectable for deterministic tests. */
export function makeContent(input: ContentInput, now: number = Date.now()): Content {
  const content: Content = {
    version: PROTOCOL_VERSION,
    intent: input.intent,
    thread_id: input.threadId ?? randomUUID(),
  }
  if (input.replyTo) content.reply_to = input.replyTo
  if (input.topic) content.topic = input.topic
  if (input.tags?.length) content.tags = input.tags
  if (input.payload) content.payload = input.payload
  if (input.ttlSeconds != null) content.expires_at = Math.floor(now / 1000) + input.ttlSeconds
  if (input.status) content.status = input.status
  if (input.error !== undefined) content.error = input.error
  return content
}
