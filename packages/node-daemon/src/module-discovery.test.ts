import { describe, it, expect } from 'vitest'
import { discoverModules, OFFICIAL_SCOPES, type RegistryFetch } from './module-discovery'

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
