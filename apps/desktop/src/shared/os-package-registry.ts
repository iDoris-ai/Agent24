// Domain OS packages the desktop may download. The registry names the GitHub
// release; trust is not this file. SHA256SUMS only detects a broken download.
// `agent24 os install` checks the Developer ID signature and files.json.

/** The only domain OS this build downloads. Kept in code so the desktop bundle does not depend on a loose JSON file. */
export const OS_PACKAGE_REGISTRY = {
  sin90: {
    name: 'Sin90',
    repo: 'iDoris-ai/Sin90',
    assetTemplate: 'sin90-{version}-macos-{arch}.tar.gz',
    checksumFile: 'SHA256SUMS',
  },
} as const

export const SIN90_REPO = 'iDoris-ai/Sin90'
export const SIN90_DOWNLOAD_PREFIX = 'https://github.com/iDoris-ai/Sin90/releases/download/'
const RELEASE_API = 'https://api.github.com/repos/iDoris-ai/Sin90/releases/latest'
const ASSET_TEMPLATE = 'sin90-{version}-macos-{arch}.tar.gz'
const CHECKSUM_FILE = 'SHA256SUMS'
const MAC_ARCH = new Set(['arm64', 'x64'])

export interface OsPackageEntry {
  id: string
  name: string
  repo: string
  assetTemplate: string
  checksumFile: string
  releaseApi: string
  downloadPrefix: string
}

export interface ReleaseAsset {
  version: string
  tag: string
  assetName: string
  assetUrl: string
  checksumName: string
  checksumUrl: string
  bytes: number
}

interface RawEntry {
  name?: unknown
  repo?: unknown
  assetTemplate?: unknown
  checksumFile?: unknown
}

function rawEntries(): Record<string, RawEntry> {
  return OS_PACKAGE_REGISTRY
}

/** A registry row is usable only when it is the Sin90 release this build knows. */
export function osPackageEntry(id: string): OsPackageEntry | null {
  if (id !== 'sin90') return null
  const row = rawEntries()[id]
  if (!row || row.name !== 'Sin90' || row.repo !== SIN90_REPO || row.assetTemplate !== ASSET_TEMPLATE || row.checksumFile !== CHECKSUM_FILE) {
    return null
  }
  return {
    id,
    name: 'Sin90',
    repo: SIN90_REPO,
    assetTemplate: ASSET_TEMPLATE,
    checksumFile: CHECKSUM_FILE,
    releaseApi: RELEASE_API,
    downloadPrefix: SIN90_DOWNLOAD_PREFIX,
  }
}

export function isRegisteredOsPackage(id: string): boolean {
  return osPackageEntry(id) !== null
}

/** `vX.Y.Z` only. Pre-releases and bare numbers are not a latest release. */
export function releaseVersion(tag: string): string | null {
  const match = /^v(\d+\.\d+\.\d+)$/.exec(tag)
  return match?.[1] ?? null
}

/** Negative when `a` is older. NaN when either side is not X.Y.Z. */
export function compareVersions(a: string, b: string): number {
  const parts = (value: string): number[] | null => {
    const nums = value.replace(/^v/, '').split('.')
    if (nums.length !== 3) return null
    const parsed = nums.map((part) => Number(part))
    if (parsed.some((n) => !Number.isInteger(n) || n < 0)) return null
    return parsed
  }
  const left = parts(a)
  const right = parts(b)
  if (!left || !right) return Number.NaN
  for (let i = 0; i < 3; i += 1) {
    if (left[i] !== right[i]) return left[i]! - right[i]!
  }
  return 0
}

export function assetNameFor(version: string, arch: string): string {
  return ASSET_TEMPLATE.replace('{version}', version).replace('{arch}', arch)
}

/** The download URL we will request. Anything else in the release JSON is ignored. */
export function expectedDownloadUrl(tag: string, fileName: string): string {
  return `${SIN90_DOWNLOAD_PREFIX}${tag}/${fileName}`
}

export function isAllowedDownloadUrl(url: string, tag: string, fileName: string): boolean {
  let parsed: URL
  try {
    parsed = new URL(url)
  } catch {
    return false
  }
  if (parsed.protocol !== 'https:' || parsed.username !== '' || parsed.password !== '') return false
  if (parsed.hostname !== 'github.com') return false
  if (parsed.search !== '' || parsed.hash !== '') return false
  return parsed.pathname === `/iDoris-ai/Sin90/releases/download/${tag}/${fileName}`
}

/** GitHub's release CDN. The URL we chose is still the github.com download URL. */
const REDIRECT_HOSTS = new Set([
  'release-assets.githubusercontent.com',
  'objects.githubusercontent.com',
  'github-releases.githubusercontent.com',
])

export function isAllowedDownloadRedirect(url: string): boolean {
  let parsed: URL
  try {
    parsed = new URL(url)
  } catch {
    return false
  }
  return parsed.protocol === 'https:' && parsed.username === '' && parsed.password === '' && REDIRECT_HOSTS.has(parsed.hostname)
}

interface GitHubAsset {
  name?: unknown
  browser_download_url?: unknown
  size?: unknown
}

/** Reads `releases/latest`. Rejects a tag, asset, or URL outside the registry. */
export function parseLatestRelease(body: unknown, arch: string): ReleaseAsset | null {
  if (!MAC_ARCH.has(arch)) return null
  if (typeof body !== 'object' || body === null) return null
  const record = body as { tag_name?: unknown; assets?: unknown }
  if (typeof record.tag_name !== 'string' || !Array.isArray(record.assets)) return null
  const version = releaseVersion(record.tag_name)
  if (version === null) return null
  const assetName = assetNameFor(version, arch)
  const checksumName = CHECKSUM_FILE
  const asset = findAsset(record.assets, assetName, record.tag_name)
  const checksum = findAsset(record.assets, checksumName, record.tag_name)
  if (!asset || asset.bytes <= 0 || !checksum) return null
  return {
    version,
    tag: record.tag_name,
    assetName,
    assetUrl: asset.url,
    checksumName,
    checksumUrl: checksum.url,
    bytes: asset.bytes,
  }
}

function findAsset(assets: unknown[], fileName: string, tag: string): { url: string; bytes: number } | null {
  for (const value of assets) {
    if (typeof value !== 'object' || value === null) continue
    const asset = value as GitHubAsset
    if (asset.name !== fileName || typeof asset.browser_download_url !== 'string') continue
    if (!isAllowedDownloadUrl(asset.browser_download_url, tag, fileName)) return null
    if (typeof asset.size !== 'number' || !Number.isFinite(asset.size) || asset.size < 0) return null
    return { url: asset.browser_download_url, bytes: asset.size }
  }
  return null
}

/** One `SHA256SUMS` line for `assetName`: 64 hex digits, then two spaces or a space and `*`. */
export function checksumForAsset(text: string, assetName: string): string | null {
  for (const line of text.split('\n')) {
    const match = /^([0-9a-fA-F]{64}) (\*| )(.+)$/.exec(line.trim())
    if (match?.[3] === assetName) return match[1]!.toLowerCase()
  }
  return null
}
