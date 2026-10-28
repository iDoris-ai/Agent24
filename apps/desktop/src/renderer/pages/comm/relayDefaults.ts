// Fill this when Jason supplies the AAStar relay address.
export const AASTAR_RELAY_URL: string = ''

const DEFAULT_RELAYS = {
  AAStar: AASTAR_RELAY_URL,
  'iDoris (agent-speaker-relay)': 'wss://relay.aastar.io',
}

export function getDefaultRelays(aastarUrl = AASTAR_RELAY_URL) {
  return Object.entries({ ...DEFAULT_RELAYS, AAStar: aastarUrl })
    .filter(([, url]) => url !== '')
    .map(([name, url]) => ({ name, url }))
}
