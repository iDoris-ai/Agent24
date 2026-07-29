// M4 marketplace — module DISCOVERY: find installable modules from the npm
// registry (distinct from module-installer's `discoverInstalledModules`, which
// lists what is already installed locally). This is the backend the marketplace
// browse UI and one-click install build on; the authoritative permission
// consent still happens at install time (H10 `consentSummary`), so discovery
// only surfaces enough to browse and choose: name, version, description, trust
// tier, and whether it's already installed.

/** Trust tier shown in the marketplace (roadmap: 官方 / 社区 / 第三方). */
export type TrustTier = 'official' | 'community' | 'third-party'

export interface DiscoveredModule {
  packageName: string
  version: string
  name: string
  description: string
  trustTier: TrustTier
  installed: boolean
}

/** Injectable so tests never hit the network; defaults to the public npm
 * registry search API. */
export type RegistryFetch = (url: string) => Promise<unknown>

/** agent24's own npm scopes — modules published here are `official`. */
export const OFFICIAL_SCOPES = ['@auraaihq', '@agent24'] as const

const defaultFetch: RegistryFetch = async (url) => {
  const res = await fetch(url)
  if (!res.ok) throw new Error(`registry HTTP ${res.status}`)
  return res.json()
}

interface SearchObject {
  package?: { name?: string; version?: string; description?: string }
}

function tierFor(scope: string, officialScopes: readonly string[]): TrustTier {
  return officialScopes.includes(scope) ? 'official' : 'community'
}

/** Discover installable modules by scanning npm scopes. One scope failing (e.g.
 * registry unreachable) is logged and skipped — never fails the whole call, so
 * the marketplace degrades to "what we could reach" rather than an error. */
export async function discoverModules(
  opts: {
    /** npm scopes to scan. Defaults to the official scopes. */
    scopes?: readonly string[]
    /** Which scanned scopes count as `official`. Defaults to {@link OFFICIAL_SCOPES}. */
    officialScopes?: readonly string[]
    fetch?: RegistryFetch
    /** Locally-installed package names → the `installed` flag. */
    installed?: ReadonlySet<string>
  } = {},
): Promise<DiscoveredModule[]> {
  const scopes = opts.scopes ?? OFFICIAL_SCOPES
  const officialScopes = opts.officialScopes ?? OFFICIAL_SCOPES
  const doFetch = opts.fetch ?? defaultFetch
  const installed = opts.installed ?? new Set<string>()
  const byName = new Map<string, DiscoveredModule>()

  for (const scope of scopes) {
    let objects: SearchObject[] = []
    try {
      const url = `https://registry.npmjs.org/-/v1/search?text=${encodeURIComponent(`scope:${scope}`)}&size=100`
      const res = await doFetch(url)
      objects = (res as { objects?: SearchObject[] })?.objects ?? []
    } catch (err) {
      console.error(
        `[modules] discover scope ${scope} failed:`,
        err instanceof Error ? err.message : err,
      )
      continue
    }
    for (const o of objects) {
      const name = o.package?.name
      if (!name || byName.has(name)) continue // first scope wins on duplicates
      byName.set(name, {
        packageName: name,
        version: o.package?.version ?? '',
        name,
        description: o.package?.description ?? '',
        trustTier: tierFor(scope, officialScopes),
        installed: installed.has(name),
      })
    }
  }
  return [...byName.values()].sort((a, b) => a.packageName.localeCompare(b.packageName))
}
