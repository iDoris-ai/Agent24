// Owner decision 2026-10-05: Open Design is no longer bundled inside the
// desktop installer (it alone grew the AppImage from 146MB to 934MB and the
// deb from 101MB to 622MB). electron-builder now only bakes a small manifest
// (`open-design-component.json`, produced by
// scripts/pack-open-design-component.mjs) describing a downloadable tarball.
// This module is the runtime installer: it turns that manifest into an
// installed `app/ open-design/ open-design-web-standalone/` tree under
// `~/.agent24/components/open-design/<odSha7>-<platform>-<arch>/`, the exact
// layout `CreativeServeWeb` already expects from `resourcesPath`.

import crypto from 'node:crypto'
import { execFile } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { Readable } from 'node:stream'
import { promisify } from 'node:util'

const execFileAsync = promisify(execFile)

export const OPEN_DESIGN_COMPONENT_MANIFEST_FILE = 'open-design-component.json'
const RESOURCE_TREES = ['app', 'open-design', 'open-design-web-standalone'] as const
const REQUIRED_FILES = [
  'app/package.json',
  'app/prebundled/agent24-headless.cjs',
  'app/prebundled/agent24-headless.mjs',
  'app/prebundled/daemon/daemon-cli.mjs',
  'app/prebundled/daemon/daemon-sidecar.mjs',
  'app/prebundled/web-sidecar.mjs',
]

export interface OpenDesignComponentManifest {
  version: string
  odSha: string
  platform: string
  arch: string
  url: string
  sha256: string
  size: number
}

export type OpenDesignComponentState =
  | 'not-installed'
  | 'downloading'
  | 'verifying'
  | 'installed'
  | 'failed'
  | 'unavailable'

export interface OpenDesignComponentStatus {
  state: OpenDesignComponentState
  received?: number
  total?: number
  size?: number
  dir?: string
  error?: string
  reason?: string
}

type FetchFn = typeof fetch

function isNonEmptyString(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0
}

/** Defensive shape check even for the manifest baked at build time — a hand
 * edited or truncated build artifact must fail loudly, not silently install
 * garbage. */
export function parseOpenDesignComponentManifest(raw: unknown): OpenDesignComponentManifest {
  if (raw == null || typeof raw !== 'object' || Array.isArray(raw)) {
    throw new Error('Open Design component manifest must be an object')
  }
  const value = raw as Record<string, unknown>
  if (!isNonEmptyString(value.version)) throw new Error('Open Design component manifest version is invalid')
  if (!isNonEmptyString(value.odSha) || !/^[0-9a-f]{40}$/.test(value.odSha)) {
    throw new Error('Open Design component manifest odSha is invalid')
  }
  if (!isNonEmptyString(value.platform)) throw new Error('Open Design component manifest platform is invalid')
  if (!isNonEmptyString(value.arch)) throw new Error('Open Design component manifest arch is invalid')
  if (!isNonEmptyString(value.url)) throw new Error('Open Design component manifest url is invalid')
  if (!isNonEmptyString(value.sha256) || !/^[0-9a-f]{64}$/.test(value.sha256)) {
    throw new Error('Open Design component manifest sha256 is invalid')
  }
  if (typeof value.size !== 'number' || !Number.isInteger(value.size) || value.size <= 0) {
    throw new Error('Open Design component manifest size is invalid')
  }
  return {
    version: value.version,
    odSha: value.odSha,
    platform: value.platform,
    arch: value.arch,
    url: value.url,
    sha256: value.sha256,
    size: value.size,
  }
}

/** Reads the manifest electron-builder baked into `resourcesPath` at build
 * time. Returns `null` for a dev/unreleased build that never baked one — the
 * caller surfaces that as the `unavailable` state, not `failed`. */
export function loadOpenDesignComponentManifest(resourcesPath?: string): OpenDesignComponentManifest | null {
  if (!resourcesPath) return null
  const manifestPath = path.join(resourcesPath, OPEN_DESIGN_COMPONENT_MANIFEST_FILE)
  if (!fs.existsSync(manifestPath)) return null
  return parseOpenDesignComponentManifest(JSON.parse(fs.readFileSync(manifestPath, 'utf8')))
}

export function openDesignComponentInstallDir(componentsRoot: string, manifest: OpenDesignComponentManifest): string {
  return path.join(componentsRoot, `${manifest.odSha.slice(0, 7)}-${manifest.platform}-${manifest.arch}`)
}

/** Cheap startup re-validation: required files present, nothing more. The
 * full symlink-safety walk only runs right after a fresh extract — see
 * `verifyComponentContract`. */
export function quickValidateInstalledComponent(dir: string): boolean {
  if (!fs.existsSync(dir)) return false
  return REQUIRED_FILES.every((relative) => fs.statSync(path.join(dir, relative), { throwIfNoEntry: false })?.isFile())
}

function isInsideResourceTrees(root: string, target: string): boolean {
  return RESOURCE_TREES.some((tree) => {
    const treeRoot = path.join(root, tree)
    return target === treeRoot || target.startsWith(`${treeRoot}${path.sep}`)
  })
}

function verifySymlinksRecursive(root: string, directory: string): void {
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const fullPath = path.join(directory, entry.name)
    if (entry.isSymbolicLink()) {
      const link = fs.readlinkSync(fullPath)
      if (path.isAbsolute(link)) throw new Error(`Open Design component has an absolute symlink: ${fullPath}`)
      const resolved = fs.realpathSync(fullPath)
      if (!isInsideResourceTrees(root, resolved)) {
        throw new Error(`Open Design component symlink escapes its resource trees: ${fullPath}`)
      }
      continue
    }
    if (entry.isDirectory()) verifySymlinksRecursive(root, fullPath)
  }
}

/** Ported from `scripts/stage-open-design-resources.mjs`'s build-time
 * contract — required files, pinned version, and symlink safety — run once
 * against a freshly extracted tarball before it is trusted and renamed into
 * place. */
export function verifyComponentContract(root: string, expectedVersion: string): void {
  for (const tree of RESOURCE_TREES) {
    const treeRoot = path.join(root, tree)
    const stat = fs.lstatSync(treeRoot, { throwIfNoEntry: false })
    if (stat?.isSymbolicLink()) throw new Error(`Open Design component resource tree root must not be a symlink: ${tree}`)
    if (!stat?.isDirectory()) throw new Error(`Open Design component resource tree is missing: ${tree}`)
  }
  for (const relative of REQUIRED_FILES) {
    if (!fs.statSync(path.join(root, relative), { throwIfNoEntry: false })?.isFile()) {
      throw new Error(`Open Design component is missing a required file: ${relative}`)
    }
  }
  const manifest = JSON.parse(fs.readFileSync(path.join(root, 'app/package.json'), 'utf8')) as { version?: unknown }
  if (manifest.version !== expectedVersion) {
    throw new Error(`Open Design component version mismatch: expected ${expectedVersion}, got ${String(manifest.version)}`)
  }
  for (const tree of RESOURCE_TREES) verifySymlinksRecursive(root, path.join(root, tree))
}

export interface OpenDesignComponentInstallerOptions {
  manifest?: OpenDesignComponentManifest | null
  resourcesPath?: string
  componentsRoot?: string
  tarBinary?: string
}

export class OpenDesignComponentInstaller {
  private readonly manifest: OpenDesignComponentManifest | null
  private readonly componentsRoot: string
  private readonly tarBinary: string
  private readonly fetchFn: FetchFn
  private current: OpenDesignComponentStatus
  private installing: Promise<OpenDesignComponentStatus> | null = null
  private readonly listeners = new Set<(status: OpenDesignComponentStatus) => void>()

  constructor(options: OpenDesignComponentInstallerOptions = {}, fetchFn: FetchFn = fetch) {
    this.manifest = options.manifest !== undefined ? options.manifest : loadOpenDesignComponentManifest(options.resourcesPath)
    this.componentsRoot = options.componentsRoot ?? path.join(os.homedir(), '.agent24', 'components', 'open-design')
    this.tarBinary = options.tarBinary ?? 'tar'
    this.fetchFn = fetchFn
    this.current = this.computeInitialStatus()
  }

  private computeInitialStatus(): OpenDesignComponentStatus {
    if (!this.manifest) {
      return { state: 'unavailable', reason: 'This build does not bundle an Open Design component manifest' }
    }
    const dir = openDesignComponentInstallDir(this.componentsRoot, this.manifest)
    if (quickValidateInstalledComponent(dir)) return { state: 'installed', dir }
    return { state: 'not-installed', size: this.manifest.size }
  }

  status(): OpenDesignComponentStatus {
    return { ...this.current }
  }

  onProgress(callback: (status: OpenDesignComponentStatus) => void): () => void {
    this.listeners.add(callback)
    return () => this.listeners.delete(callback)
  }

  private setStatus(status: OpenDesignComponentStatus): void {
    this.current = status
    for (const listener of this.listeners) listener(this.status())
  }

  install(): Promise<OpenDesignComponentStatus> {
    if (!this.manifest) return Promise.resolve(this.status())
    if (this.current.state === 'installed') return Promise.resolve(this.status())
    if (this.installing) return this.installing
    const pending = this.installOnce(this.manifest)
    this.installing = pending
    pending.then(
      () => { if (this.installing === pending) this.installing = null },
      () => { if (this.installing === pending) this.installing = null },
    )
    return pending
  }

  private async installOnce(manifest: OpenDesignComponentManifest): Promise<OpenDesignComponentStatus> {
    if (!manifest.url.startsWith('https://')) {
      this.setStatus({ state: 'failed', error: 'Open Design component manifest URL must use HTTPS' })
      return this.status()
    }

    fs.mkdirSync(this.componentsRoot, { recursive: true })
    const token = crypto.randomBytes(6).toString('hex')
    const partialDir = path.join(this.componentsRoot, `.partial-${token}`)
    const tarballPath = path.join(this.componentsRoot, `.partial-${token}.tar.gz`)

    try {
      this.setStatus({ state: 'downloading', received: 0, total: manifest.size })
      await this.download(manifest, tarballPath)

      this.setStatus({ state: 'verifying' })
      fs.mkdirSync(partialDir, { recursive: true })
      await execFileAsync(this.tarBinary, ['-xzf', tarballPath, '-C', partialDir])
      verifyComponentContract(partialDir, manifest.version)

      const finalDir = openDesignComponentInstallDir(this.componentsRoot, manifest)
      fs.rmSync(finalDir, { recursive: true, force: true })
      fs.renameSync(partialDir, finalDir)
      this.setStatus({ state: 'installed', dir: finalDir })
      return this.status()
    } catch (error) {
      fs.rmSync(partialDir, { recursive: true, force: true })
      const message = error instanceof Error ? error.message : String(error)
      this.setStatus({ state: 'failed', error: message })
      return this.status()
    } finally {
      fs.rmSync(tarballPath, { force: true })
    }
  }

  private async download(manifest: OpenDesignComponentManifest, destination: string): Promise<void> {
    const response = await this.fetchFn(manifest.url)
    if (!response.ok) throw new Error(`Open Design component download failed: HTTP ${response.status}`)
    if (!response.body) throw new Error('Open Design component download returned no body')

    const hash = crypto.createHash('sha256')
    let received = 0
    const file = fs.createWriteStream(destination)
    // We handle every failure ourselves (via the catch below and the
    // caller's try/catch); without this, a stray write/close error after we
    // have already moved on would surface as an unhandled 'error' event and
    // crash the process (Node's EventEmitter default for a listener-less
    // 'error' emission).
    file.on('error', () => {})
    const nodeStream = Readable.fromWeb(response.body as never)

    try {
      for await (const chunk of nodeStream as AsyncIterable<Buffer>) {
        received += chunk.length
        if (received > manifest.size) {
          throw new Error('Open Design component download exceeded the manifest-declared size')
        }
        hash.update(chunk)
        if (!file.write(chunk)) await new Promise<void>((resolve) => file.once('drain', () => resolve()))
        this.setStatus({ state: 'downloading', received, total: manifest.size })
      }
      await new Promise<void>((resolve, reject) => {
        file.end((error: unknown) => (error ? reject(error) : resolve()))
      })
    } catch (error) {
      // Wait for the fd to actually close before the caller deletes this
      // path — fs.WriteStream.destroy() can leave an in-flight open/write
      // that still touches the path after we return.
      await new Promise<void>((resolve) => file.close(() => resolve()))
      void response.body?.cancel?.().catch(() => {})
      throw error
    }

    if (received !== manifest.size) {
      throw new Error(`Open Design component download size mismatch: expected ${manifest.size}, got ${received}`)
    }
    const digest = hash.digest('hex')
    if (digest !== manifest.sha256) {
      throw new Error(`Open Design component sha256 mismatch: expected ${manifest.sha256}, got ${digest}`)
    }
  }
}
