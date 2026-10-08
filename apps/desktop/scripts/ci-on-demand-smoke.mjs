// M2 (review finding, 2026-10-05): closes the loop CI never actually
// checked — that the on-demand Open Design component, once genuinely
// downloaded and installed by OpenDesignComponentInstaller, can actually
// be launched by CreativeServeWeb through the REAL packaged Electron
// binary (ELECTRON_RUN_AS_NODE=1) and reach a real /api/ready.
//
// What this proves: the real install path (HTTPS/allowlist checks, sha256
// verification, tar member safety checks, extraction, contract
// verification) landing in the real default `~/.agent24/components/
// open-design/` directory; the real CreativeServeWeb spawn args/cwd/env
// (including ELECTRON_RUN_AS_NODE); the real stdout ready-line contract
// (protocol v2); and a real HTTP /api/ready poll succeeding end to end.
//
// What this does NOT prove: the fork's (iDoris-ai/open-design-agent24)
// own protocol v2 implementation — this script runs a local stand-in
// launcher (fixtures/ci-on-demand-smoke-launcher.cjs) instead of the real
// agent24-headless.cjs, because the officially pinned Open Design commit
// (OPEN_DESIGN_SHA in release-desktop.yml) still only speaks protocol v1,
// and the pin is deliberately not bumped yet pending fork PR #7 merging.
// See that fixture's own header comment for the full reasoning, and
// delete both it and this carve-out once the pin moves past the merge.
//
// Run from apps/desktop (same working directory electron-builder's other
// scripts use): `node scripts/ci-on-demand-smoke.mjs`.
//
// Required env:
//   A24_SMOKE_RUNTIME_EXECUTABLE — absolute path to the real packaged
//     Electron binary to launch (platform-specific: AppImage-extracted
//     Agent24 binary on Linux, Contents/MacOS/Agent24 on mac).
// Optional env (local dry runs only — CI never sets this, so CI always
// exercises the real default ~/.agent24/components/open-design path):
//   A24_SMOKE_COMPONENTS_ROOT — overrides the installer's componentsRoot.

import crypto from 'node:crypto'
import { execFile } from 'node:child_process'
import fs from 'node:fs'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { promisify } from 'node:util'

const execFileAsync = promisify(execFile)
const scriptsDir = path.dirname(fileURLToPath(import.meta.url))
const desktopDir = path.resolve(scriptsDir, '..')

function fail(message) {
  process.stderr.write(`ci-on-demand-smoke: ${message}\n`)
  process.exit(1)
}

const runtimeExecutable = process.env.A24_SMOKE_RUNTIME_EXECUTABLE
if (!runtimeExecutable || !path.isAbsolute(runtimeExecutable)) {
  fail('A24_SMOKE_RUNTIME_EXECUTABLE must be set to an absolute path to the real packaged Electron binary')
}
if (!fs.existsSync(runtimeExecutable)) {
  fail(`A24_SMOKE_RUNTIME_EXECUTABLE does not exist: ${runtimeExecutable}`)
}

const { OpenDesignComponentInstaller } = await import(path.join(desktopDir, 'dist/main/open-design-component.js'))
const { CreativeServeWeb } = await import(path.join(desktopDir, 'dist/main/creative-serve-web.js'))

function mkdtemp(prefix) {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix))
}

function buildFakeComponentTarball() {
  const stage = mkdtemp('a24-od-smoke-stage-')
  for (const relative of ['app/prebundled/daemon', 'open-design', 'open-design-web-standalone']) {
    fs.mkdirSync(path.join(stage, relative), { recursive: true })
  }
  fs.writeFileSync(path.join(stage, 'app/package.json'), JSON.stringify({ version: '0.22.2' }))
  fs.copyFileSync(
    path.join(scriptsDir, 'fixtures/ci-on-demand-smoke-launcher.cjs'),
    path.join(stage, 'app/prebundled/agent24-headless.cjs'),
  )
  for (const relative of [
    // Only the .cjs file is ever actually executed (see
    // resolveAgent24HeadlessLauncher) — the rest just need to exist to
    // satisfy verifyComponentContract's REQUIRED_FILES check.
    'app/prebundled/agent24-headless.mjs',
    'app/prebundled/daemon/daemon-cli.mjs',
    'app/prebundled/daemon/daemon-sidecar.mjs',
    'app/prebundled/web-sidecar.mjs',
  ]) fs.writeFileSync(path.join(stage, relative), '')
  fs.writeFileSync(path.join(stage, 'open-design-web-standalone/server.js'), '')

  const tarPath = path.join(stage, '..', `${path.basename(stage)}.tar.gz`)
  return execFileAsync('tar', ['-czf', tarPath, '-C', stage, 'app', 'open-design', 'open-design-web-standalone'])
    .then(() => tarPath)
}

async function sha256File(filePath) {
  const hash = crypto.createHash('sha256')
  for await (const chunk of fs.createReadStream(filePath)) hash.update(chunk)
  return hash.digest('hex')
}

async function startFixtureServer(tarballPath) {
  const bytes = fs.readFileSync(tarballPath)
  const server = http.createServer((_req, res) => {
    res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-length': String(bytes.length) })
    res.end(bytes)
  })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  const { port } = server.address()
  return { server, port }
}

async function main() {
  console.log('[1/5] building a fake-but-contract-valid Open Design component tarball...')
  const tarballPath = await buildFakeComponentTarball()
  const sha256 = await sha256File(tarballPath)
  const size = fs.statSync(tarballPath).size
  console.log(`      tarball: ${tarballPath} (${size} bytes, sha256 ${sha256})`)

  console.log('[2/5] serving it from a local HTTP fixture server...')
  const { server, port } = await startFixtureServer(tarballPath)

  try {
    const manifest = {
      version: '0.22.2',
      // A fake, run-unique odSha (any 40 hex chars satisfy the manifest
      // schema) — never collides with a real installed component dir.
      odSha: crypto.randomBytes(20).toString('hex'),
      platform: process.platform,
      arch: process.arch,
      // "https" here is cosmetic: the installer requires an https:// URL
      // before it will even look at allowedHosts, and the fetch shim below
      // rewrites it straight back to this local http fixture server — the
      // same shim the unit tests use, see open-design-component.test.ts's
      // own header comment for why that's a reasonable stand-in for real
      // TLS in a test that is not trying to prove TLS itself works.
      url: `https://127.0.0.1:${port}/asset.tar.gz`,
      sha256,
      size,
    }
    const localFetch = (input, init) => fetch(String(input).replace(/^https:/, 'http:'), init)

    const componentsRoot = process.env.A24_SMOKE_COMPONENTS_ROOT || undefined
    console.log(`[3/5] installing via the real OpenDesignComponentInstaller into ${
      componentsRoot ?? '~/.agent24/components/open-design (real default)'
    }...`)
    const installer = new OpenDesignComponentInstaller(
      { manifest, allowedHosts: ['127.0.0.1'], ...(componentsRoot ? { componentsRoot } : {}) },
      localFetch,
    )
    const installResult = await installer.install()
    if (installResult.state !== 'installed' || !installResult.dir) {
      fail(`install did not reach 'installed': ${JSON.stringify(installResult)}`)
    }
    console.log(`      installed at ${installResult.dir}`)

    console.log('[4/5] launching the real CreativeServeWeb packaged-headless path through the real Electron binary...')
    const stateRoot = mkdtemp('a24-od-smoke-state-')
    const service = new CreativeServeWeb({
      stateRoot,
      runtimeExecutable,
      readyTimeoutMs: 20_000,
      component: { status: () => ({ state: 'installed', dir: installResult.dir }) },
    })
    const startResult = await service.start()
    if (startResult.state !== 'ready' || !startResult.origin) {
      fail(`CreativeServeWeb did not reach 'ready': ${JSON.stringify(startResult)}`)
    }
    console.log(`      ready at ${startResult.origin}`)

    console.log('[5/5] double-checking /api/ready directly (CreativeServeWeb already polled this to get here)...')
    const response = await fetch(`${startResult.origin}/api/ready`)
    if (!response.ok) fail(`/api/ready returned ${response.status}`)
    console.log(`      /api/ready -> ${response.status}`)

    await service.stop()
    console.log('ci-on-demand-smoke: OK — on-demand install -> real packaged launch -> /api/ready all passed')
  } finally {
    await new Promise((resolve) => server.close(resolve))
  }
}

main().catch((error) => {
  fail(error instanceof Error ? (error.stack ?? error.message) : String(error))
})
