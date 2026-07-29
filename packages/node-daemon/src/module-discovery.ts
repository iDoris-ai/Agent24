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

/** Trust tier from the package's OWN scope (parsed from its name), NOT from the
 * scope we happened to query. npm's `scope:` search qualifier is a hint, not a
 * guarantee — deriving `official` from the queried scope would let an
 * out-of-scope package (or a future user-added scan scope) be stamped official
 * by association. The package name is the authority. */
function tierForName(name: string, officialScopes: readonly string[]): TrustTier {
  const pkgScope = name.startsWith('@') ? (name.split('/')[0] ?? '') : ''
  return pkgScope && officialScopes.includes(pkgScope) ? 'official' : 'community'
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
        trustTier: tierForName(name, officialScopes),
        installed: installed.has(name),
      })
    }
  }
  return [...byName.values()].sort((a, b) => a.packageName.localeCompare(b.packageName))
}

/** Marketplace browse filters (ROADMAP: 浏览面板 搜索 + 过滤). All optional and
 * ANDed together; an absent field doesn't constrain. */
export interface ModuleFilter {
  /** Case-insensitive substring, matched across packageName + name + description. */
  query?: string
  /** Keep only this trust tier. */
  trustTier?: TrustTier
  /** `true` → only already-installed; `false` → only not-installed; absent → both. */
  installed?: boolean
}

const TRUST_TIERS: readonly TrustTier[] = ['official', 'community', 'third-party']

/** Narrow a filter value from an untrusted query string. Returns undefined for
 * an unknown tier so a bad `?tier=` degrades to "no tier filter" rather than
 * silently matching nothing. */
export function parseTrustTier(value: string | undefined): TrustTier | undefined {
  return value && (TRUST_TIERS as readonly string[]).includes(value)
    ? (value as TrustTier)
    : undefined
}

/** Parse the `installed` query param: only the explicit strings 'true'/'false'
 * constrain; anything else (including absent) means "don't filter by installed". */
export function parseInstalledFilter(value: string | undefined): boolean | undefined {
  if (value === 'true') return true
  if (value === 'false') return false
  return undefined
}

/** Apply browse filters to a discovered-module list. Pure over its input — the
 * network fetch already happened in {@link discoverModules}; this is what the
 * marketplace search box + tier/installed toggles drive. */
export function filterModules(
  modules: readonly DiscoveredModule[],
  filter: ModuleFilter = {},
): DiscoveredModule[] {
  const q = filter.query?.trim().toLowerCase()
  return modules.filter((m) => {
    if (filter.trustTier && m.trustTier !== filter.trustTier) return false
    if (filter.installed !== undefined && m.installed !== filter.installed) return false
    if (q) {
      const hay = `${m.packageName} ${m.name} ${m.description}`.toLowerCase()
      if (!hay.includes(q)) return false
    }
    return true
  })
}
