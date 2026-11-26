// KB-LITE-03a: the ONE place that pins which WeKnora Lite build this App
// installs. Trust root = these values, compiled into the App's main bundle
// (so they ship inside the signed App, exactly like Open Design's baked
// sha256). The installer never trusts anything it downloads beyond what is
// written here.
//
// How to change the pin (e.g. after the L9-2 hardening re-build):
//   1. Build with WeKnora `scripts/agent24/build-lite-headless.sh <sha> --out <dir> --sign-key <key>`.
//   2. Check it: `scripts/agent24/verify-lite-headless.sh <manifest> <archive> <allowed_signers> <sha>` prints `OK`.
//   3. Copy source_sha, version, os, arch, archive, archive_sha256, binary_sha256,
//      license, license_sha256, third_party_notices_sha256 from the manifest
//      below, and archiveSize from `stat -f %z <archive>`.
// No upgrade path: changing sourceSha installs into a new
// `<sha7>-darwin-arm64` dir; old dirs are not migrated or removed (FU).

export interface WeknoraLitePin {
  /** WeKnora fork commit the artifact was built from (40 hex). */
  sourceSha: string
  version: string
  /** Go GOOS/GOARCH — same strings as Node's process.platform/arch here. */
  os: string
  arch: string
  /** Archive file name; also the asset name under a download base URL. */
  archive: string
  archiveSha256: string
  archiveSize: number
  binarySha256: string
  /** First line of the bundled LICENSE (informational). */
  license: string
  licenseSha256: string
  noticesSha256: string
  /** Official https download URL. `null` = not published (Loop 9: David
   * 11-26 decided no public distribution yet); only the loopback dev
   * override (A24_WEKNORA_LITE_LOCAL_BASE_URL) can then install it. */
  url: string | null
}

// WeKnora#180 head 65b30a78 (KB-LITE-02a: handshake protocol 2 with the
// instance credential, lifeline fd). Built by L9-2 on the mini 11-27 with a
// development signing key, verified with verify-lite-headless.sh (OK). Not
// published. Re-pin if #180 merges with another sha.
export const WEKNORA_LITE_PIN: WeknoraLitePin = {
  sourceSha: '65b30a7800ea9b62e7ee2a1068f219f37b9f8af7',
  version: '0.9.0',
  os: 'darwin',
  arch: 'arm64',
  archive: 'WeKnora-lite-headless_0.9.0_65b30a7800ea_darwin_arm64.tar.gz',
  archiveSha256: 'e86a7834c27f23c30e6e92c11576c23bd7b69b8d6cbc8b9463bf37cc9cd3d0fa',
  archiveSize: 82_276_220,
  binarySha256: '5660239a26ba32b9a8f47eecd620178d898b1a63cb837569345f35fcf9c223ff',
  license: 'Tencent is pleased to support the open source community by making  WeKnora available.',
  licenseSha256: '5935f49975d7daf97857d7053b6fe45500a3b2ec31cc92a42a89a5fd98a86cea',
  noticesSha256: 'f5e311909a349f76d3776423036a29245f73052c1cf72f614dc6479ecc43b05a',
  url: null,
}
