import { describe, it, expect } from 'vitest'
import {
  discoverModules,
  filterModules,
  parseTrustTier,
  parseInstalledFilter,
  OFFICIAL_SCOPES,
  type RegistryFetch,
  type DiscoveredModule,
} from './module-discovery'

/** A fake npm registry: returns canned search objects keyed by the scope in the
 * query, so tests never touch the network. */
function fakeRegistry(byScope: Record<string, { name: string; version?: string; description?: string }[]>): RegistryFetch {
  return async (url) => {
    const scope = decodeURIComponent(url).match(/scope:(@[\w-]+)/)?.[1] ?? ''
    const pkgs = byScope[scope] ?? []
    return { objects: pkgs.map((package_) => ({ package: package_ })) }
  }
}

describe('discoverModules', () => {
  it('maps npm search results and tags official scopes', async () => {
    const fetch = fakeRegistry({
      '@auraaihq': [{ name: '@auraaihq/mod-a', version: '1.2.0', description: 'A' }],
    })
    const mods = await discoverModules({ scopes: ['@auraaihq'], fetch })
    expect(mods).toHaveLength(1)
    expect(mods[0]).toMatchObject({
      packageName: '@auraaihq/mod-a',
      version: '1.2.0',
      description: 'A',
      trustTier: 'official',
      installed: false,
    })
  })

  it('marks installed modules from the installed set', async () => {
    const fetch = fakeRegistry({
      '@auraaihq': [{ name: '@auraaihq/mod-a' }, { name: '@auraaihq/mod-b' }],
    })
    const mods = await discoverModules({
      scopes: ['@auraaihq'],
      fetch,
      installed: new Set(['@auraaihq/mod-b']),
    })
    expect(mods.find((m) => m.packageName === '@auraaihq/mod-b')?.installed).toBe(true)
    expect(mods.find((m) => m.packageName === '@auraaihq/mod-a')?.installed).toBe(false)
  })

  it('a scanned scope outside the official set is community-tier', async () => {
    const fetch = fakeRegistry({ '@someorg': [{ name: '@someorg/mod' }] })
    const mods = await discoverModules({
      scopes: ['@someorg'],
      officialScopes: OFFICIAL_SCOPES,
      fetch,
    })
    expect(mods[0]?.trustTier).toBe('community')
  })

  it('trust tier comes from the package NAME, not the queried scope (anti-spoof)', async () => {
    // npm's `scope:@auraaihq` search returns a package whose name is NOT actually
    // @auraaihq — it must not inherit `official` by association.
    const fetch = fakeRegistry({ '@auraaihq': [{ name: '@evil/lookalike' }] })
    const mods = await discoverModules({ scopes: ['@auraaihq'], fetch })
    expect(mods[0]?.trustTier).toBe('community')
  })

  it('dedupes a package that appears under multiple scanned scopes (first wins)', async () => {
    const fetch = fakeRegistry({
      '@auraaihq': [{ name: '@auraaihq/mod', description: 'official copy' }],
      '@mirror': [{ name: '@auraaihq/mod', description: 'mirror copy' }],
    })
    const mods = await discoverModules({ scopes: ['@auraaihq', '@mirror'], fetch })
    expect(mods).toHaveLength(1)
    expect(mods[0]?.description).toBe('official copy') // first scope wins
    expect(mods[0]?.trustTier).toBe('official')
  })

  it('one scope failing is skipped, not fatal — the rest still return', async () => {
    const fetch: RegistryFetch = async (url) => {
      if (url.includes('scope:@auraaihq')) throw new Error('registry HTTP 503')
      return { objects: [{ package: { name: '@agent24/mod-x' } }] }
    }
    const mods = await discoverModules({ scopes: ['@auraaihq', '@agent24'], fetch })
    expect(mods.map((m) => m.packageName)).toEqual(['@agent24/mod-x'])
  })

  it('returns [] when a scope has no packages', async () => {
    const mods = await discoverModules({ scopes: ['@auraaihq'], fetch: fakeRegistry({}) })
    expect(mods).toEqual([])
  })
})

function mod(over: Partial<DiscoveredModule>): DiscoveredModule {
  return {
    packageName: '@x/m',
    version: '1.0.0',
    name: '@x/m',
    description: '',
    trustTier: 'community',
    installed: false,
    ...over,
  }
}

describe('filterModules (marketplace browse: 搜索 + 过滤)', () => {
  const catalog: DiscoveredModule[] = [
    mod({ packageName: '@auraaihq/wechat', name: '@auraaihq/wechat', description: 'WeChat bridge', trustTier: 'official', installed: true }),
    mod({ packageName: '@auraaihq/nostr', name: '@auraaihq/nostr', description: 'Nostr channel', trustTier: 'official', installed: false }),
    mod({ packageName: '@someorg/weather', name: '@someorg/weather', description: 'Weather feed', trustTier: 'community', installed: false }),
    mod({ packageName: '@evil/miner', name: '@evil/miner', description: 'crypto miner', trustTier: 'third-party', installed: false }),
  ]

  it('no filter returns everything unchanged', () => {
    expect(filterModules(catalog)).toHaveLength(4)
  })

  it('query matches across packageName + name + description, case-insensitively', () => {
    // 'we' hits @auraaihq/WEchat, @someorg/WEather, and "WEChat bridge"
    expect(filterModules(catalog, { query: 'WE' }).map((m) => m.packageName)).toEqual([
      '@auraaihq/wechat',
      '@someorg/weather',
    ])
    // description-only hit
    expect(filterModules(catalog, { query: 'channel' }).map((m) => m.packageName)).toEqual([
      '@auraaihq/nostr',
    ])
  })

  it('trustTier keeps only the matching tier', () => {
    expect(filterModules(catalog, { trustTier: 'official' }).map((m) => m.packageName)).toEqual([
      '@auraaihq/wechat',
      '@auraaihq/nostr',
    ])
    expect(filterModules(catalog, { trustTier: 'third-party' })).toHaveLength(1)
  })

  it('installed=true/false partitions; undefined keeps both', () => {
    expect(filterModules(catalog, { installed: true }).map((m) => m.packageName)).toEqual([
      '@auraaihq/wechat',
    ])
    expect(filterModules(catalog, { installed: false })).toHaveLength(3)
    expect(filterModules(catalog, { installed: undefined })).toHaveLength(4)
  })

  it('filters AND together', () => {
    expect(
      filterModules(catalog, { trustTier: 'official', installed: false, query: 'nostr' }).map(
        (m) => m.packageName,
      ),
    ).toEqual(['@auraaihq/nostr'])
    // official + installed=true + query that only a community pkg matches → empty
    expect(filterModules(catalog, { trustTier: 'official', query: 'weather' })).toEqual([])
  })

  it('does not mutate its input', () => {
    const copy = [...catalog]
    filterModules(catalog, { trustTier: 'official' })
    expect(catalog).toEqual(copy)
  })
})

describe('parseTrustTier / parseInstalledFilter (untrusted query params)', () => {
  it('accepts the three known tiers, rejects anything else to undefined', () => {
    expect(parseTrustTier('official')).toBe('official')
    expect(parseTrustTier('community')).toBe('community')
    expect(parseTrustTier('third-party')).toBe('third-party')
    expect(parseTrustTier('OFFICIAL')).toBeUndefined() // case-sensitive enum
    expect(parseTrustTier('bogus')).toBeUndefined()
    expect(parseTrustTier(undefined)).toBeUndefined()
  })

  it('only literal "true"/"false" constrain installed; else undefined', () => {
    expect(parseInstalledFilter('true')).toBe(true)
    expect(parseInstalledFilter('false')).toBe(false)
    expect(parseInstalledFilter('1')).toBeUndefined()
    expect(parseInstalledFilter('')).toBeUndefined()
    expect(parseInstalledFilter(undefined)).toBeUndefined()
  })
})
