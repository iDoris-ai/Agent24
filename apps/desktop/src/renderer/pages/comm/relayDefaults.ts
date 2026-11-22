// Hyphae's own relay (Cloudflare Worker, run by the Hyphae project), live since 2026-11-01.
export const HYPHAE_RELAY_URL: string = 'wss://relay.aastar.io'

export function getDefaultRelays(url = HYPHAE_RELAY_URL) {
  return url === '' ? [] : [{ name: 'Hyphae (AAStar)', url }]
}

// For users who run their own relay: deploy it from this guide, then paste its address into the relay list.
export const RELAY_SELF_HOST_GUIDE_URL = 'https://github.com/iDoris-ai/agent-speaker-relay/blob/agent-speaker/RELAY-DEPLOY.md'
