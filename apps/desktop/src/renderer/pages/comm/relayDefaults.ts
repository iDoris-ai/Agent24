// Hyphae's own relay (Cloudflare Worker, run by the Hyphae project), live since 2026-11-01.
export const HYPHAE_RELAY_URL: string = 'wss://relay.aastar.io'

export function getDefaultRelays(url = HYPHAE_RELAY_URL) {
  return url === '' ? [] : [{ name: 'Hyphae (AAStar)', url }]
}
