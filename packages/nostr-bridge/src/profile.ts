// Capability abstraction (docs/specs/F4-nostr-channel.md §5): agent24 publishes
// BUSINESS capabilities ("reach the textile client segment"), never its atomic
// tools. The editable source is `agent-profile.yml` — an auto atomic layer
// (machine-generated) plus a user-editable business layer; only the business
// layer is transformed into agent-speaker's AgentProfile and published.

import yaml from 'js-yaml'

/** The editable agent24 profile file (`~/.agent24/agent-profile.yml`). */
export interface Agent24Profile {
  /** Auto layer: atomic tools/modules, machine-generated. Internal — never
   * published. Present only so the business layer can reference it. */
  atomic?: { id: string; from: string }[]
  /** Business layer: what gets published. User-editable. */
  capabilities: {
    name: string
    description?: string
    tags?: string[]
    /** Atomic ids this business capability composes (documentation only). */
    backed_by?: string[]
  }[]
  publish?: {
    mode?: 'simple' | 'tagged' | 'structured'
    availability?: string
  }
}

/** agent-speaker's on-wire AgentProfile (pkg/types/profile.go). This is the
 * shape `profile publish --json-file` reads — field names must match, e.g.
 * `rate_sheet`/`updated_at` are snake_case. */
export interface AgentSpeakerProfile {
  name: string
  mode?: string
  tags?: string[]
  description?: string
  capabilities?: { name: string; description?: string; tags?: string[] }[]
  availability?: string
  version?: string
  updated_at: number
}

/** Parse the editable YAML source. */
export function loadAgent24Profile(yamlText: string): Agent24Profile {
  const parsed = yaml.load(yamlText)
  if (!parsed || typeof parsed !== 'object' || !Array.isArray((parsed as Agent24Profile).capabilities)) {
    throw new Error('agent-profile.yml: missing a `capabilities` list')
  }
  return parsed as Agent24Profile
}

/** Transform the editable profile into agent-speaker's AgentProfile JSON —
 * BUSINESS capabilities only (atomic tools stay internal, §5). `updatedAt` is
 * a unix-seconds timestamp (injectable for deterministic tests). */
export function toAgentSpeakerProfile(
  name: string,
  p: Agent24Profile,
  updatedAt: number,
): AgentSpeakerProfile {
  const tags = [...new Set(p.capabilities.flatMap((c) => c.tags ?? []))]
  // agent-speaker (verified in 联调): `capabilities` is a STRUCTURED-mode field —
  // `tagged`/`simple` only carry name+tags and are REJECTED if capabilities are
  // set. And `profile discover --capability` only matches structured profiles.
  // So whenever we publish business capabilities the mode must be `structured`,
  // regardless of what the user wrote in `publish.mode`.
  const hasCapabilities = p.capabilities.length > 0
  const mode = hasCapabilities ? 'structured' : (p.publish?.mode ?? 'simple')
  // agent-speaker (verified in 联调): availability is an ENUM, not free text —
  // anything else (e.g. "7x24") is rejected. Only pass a valid value; otherwise
  // omit and let agent-speaker default to `available`.
  const availability =
    p.publish?.availability && (VALID_AVAILABILITY as readonly string[]).includes(p.publish.availability)
      ? p.publish.availability
      : undefined
  return {
    name,
    mode,
    ...(tags.length ? { tags } : {}),
    ...(hasCapabilities
      ? {
          capabilities: p.capabilities.map((c) => ({
            name: c.name,
            ...(c.description ? { description: c.description } : {}),
            ...(c.tags?.length ? { tags: c.tags } : {}),
          })),
        }
      : {}),
    ...(availability ? { availability } : {}),
    updated_at: updatedAt,
  }
}

/** agent-speaker's availability enum (`profile publish --availability`). */
export const VALID_AVAILABILITY = ['available', 'busy', 'away', 'offline'] as const
