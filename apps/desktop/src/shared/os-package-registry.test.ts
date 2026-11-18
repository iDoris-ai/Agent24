import { describe, expect, it } from 'vitest'
import {
  checksumForAsset,
  isAllowedDownloadUrl,
  isRegisteredOsPackage,
  parseLatestRelease,
  SIN90_DOWNLOAD_PREFIX,
} from './os-package-registry'

const TAG = 'v0.5.2'
const ASSET = 'sin90-0.5.2-macos-arm64.tar.gz'
const URL = `${SIN90_DOWNLOAD_PREFIX}${TAG}/${ASSET}`
const SUMS = `${SIN90_DOWNLOAD_PREFIX}${TAG}/SHA256SUMS`

function release(assetUrl = URL, sumsUrl = SUMS, tag = TAG) {
  return {
    tag_name: tag,
    assets: [
      { name: ASSET, browser_download_url: assetUrl, size: 1200 },
      { name: 'SHA256SUMS', browser_download_url: sumsUrl, size: 80 },
      { name: 'sin90-0.5.2-macos-x64.tar.gz', browser_download_url: `${SIN90_DOWNLOAD_PREFIX}${tag}/sin90-0.5.2-macos-x64.tar.gz`, size: 1100 },
    ],
  }
}

describe('os package registry', () => {
  it('registers only sin90', () => {
    expect(isRegisteredOsPackage('sin90')).toBe(true)
    expect(isRegisteredOsPackage('cos72')).toBe(false)
    expect(isRegisteredOsPackage('Sin90')).toBe(false)
  })

  it('accepts a vX.Y.Z asset whose URL is the Sin90 download prefix', () => {
    expect(parseLatestRelease(release(), 'arm64')).toMatchObject({ version: '0.5.2', assetUrl: URL, bytes: 1200 })
  })

  it.each([
    'https://evil.example/sin90.tar.gz',
    'https://github.com.evil/iDoris-ai/Sin90/releases/download/v0.5.2/sin90-0.5.2-macos-arm64.tar.gz',
    'https://user:pass@github.com/iDoris-ai/Sin90/releases/download/v0.5.2/sin90-0.5.2-macos-arm64.tar.gz',
    'http://github.com/iDoris-ai/Sin90/releases/download/v0.5.2/sin90-0.5.2-macos-arm64.tar.gz',
    `${URL}?redirect=https://evil.example`,
    'https://github.com/other/Sin90/releases/download/v0.5.2/sin90-0.5.2-macos-arm64.tar.gz',
  ])('rejects a download URL outside the prefix: %s', (assetUrl) => {
    expect(isAllowedDownloadUrl(assetUrl, TAG, ASSET)).toBe(false)
    expect(parseLatestRelease(release(assetUrl), 'arm64')).toBeNull()
  })

  it('rejects a tag that is not vX.Y.Z', () => {
    expect(parseLatestRelease(release(URL, SUMS, 'v0.5.2-rc.1'), 'arm64')).toBeNull()
    expect(parseLatestRelease(release(URL, SUMS, '0.5.2'), 'arm64')).toBeNull()
  })

  it('reads the SHA256SUMS line for the asset', () => {
    const hash = 'a'.repeat(64)
    expect(checksumForAsset(`${hash}  ${ASSET}\n${'b'.repeat(64)} *other\n`, ASSET)).toBe(hash)
    expect(checksumForAsset(`${hash} *${ASSET}\n`, ASSET)).toBe(hash)
    expect(checksumForAsset(`${hash}  other\n`, ASSET)).toBeNull()
  })
})
