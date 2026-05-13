// M3: Community module installer — runs npm install/uninstall in
// ~/.agent24/modules/ and dynamically loads the installed package.

import { execFile } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import os from 'node:os'
import { promisify } from 'node:util'
import type { CapabilityModule } from './capabilities/base'

const execFileAsync = promisify(execFile)

const MODULES_DIR = path.join(os.homedir(), '.agent24', 'modules')
const MODULES_PKG = path.join(MODULES_DIR, 'package.json')

// Ensure ~/.agent24/modules/ exists with a package.json so npm install works.
function ensureModulesDir(): void {
  fs.mkdirSync(MODULES_DIR, { recursive: true })
  if (!fs.existsSync(MODULES_PKG)) {
    fs.writeFileSync(MODULES_PKG, JSON.stringify({
      name: 'agent24-community-modules',
      version: '1.0.0',
      private: true,
      description: 'Agent24 community capability modules',
      dependencies: {},
    }, null, 2))
  }
}

// Validate package name: allow @scope/name and plain name formats only.
// Version specifiers (e.g. "pkg@1.0.0") are intentionally rejected — we always
// install the latest published version to keep the install path predictable.
function isValidPackageName(name: string): boolean {
  return /^(@[a-z0-9-~][a-z0-9-._~]*\/)?[a-z0-9-~][a-z0-9-._~]*$/.test(name)
}

export async function installModule(packageName: string): Promise<{ ok: boolean; modulePath?: string; error?: string }> {
  if (!isValidPackageName(packageName)) {
    return { ok: false, error: `Invalid package name: ${packageName}` }
  }

  try {
    ensureModulesDir()

    // Run npm install (use system npm; Electron bundles Node but not npm)
    await execFileAsync('npm', ['install', packageName, '--save'], {
      cwd: MODULES_DIR,
      timeout: 120_000,
      env: { ...process.env, NODE_ENV: 'production' },
    })

    // Resolve the installed package path
    const modulePath = path.join(MODULES_DIR, 'node_modules', packageName)

    if (!fs.existsSync(modulePath)) {
      return { ok: false, error: `Package installed but module path not found: ${modulePath}` }
    }

    return { ok: true, modulePath }
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err)
    return { ok: false, error: `npm install failed: ${msg}` }
  }
}

export async function uninstallModule(packageName: string): Promise<{ ok: boolean; error?: string }> {
  if (!isValidPackageName(packageName)) {
    return { ok: false, error: `Invalid package name: ${packageName}` }
  }

  try {
    ensureModulesDir()
    await execFileAsync('npm', ['uninstall', packageName, '--save'], {
      cwd: MODULES_DIR,
      timeout: 60_000,
    })
    return { ok: true }
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err)
    return { ok: false, error: `npm uninstall failed: ${msg}` }
  }
}

// Load a CapabilityModule from an installed package path.
// The package must export { manifest, register } at its main entry.
export function loadInstalledModule(modulePath: string): CapabilityModule | null {
  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const mod = require(modulePath) as { manifest?: unknown; register?: unknown }
    if (
      mod &&
      typeof mod.manifest === 'object' && mod.manifest !== null &&
      typeof mod.register === 'function'
    ) {
      return mod as CapabilityModule
    }
    return null
  } catch (err) {
    console.error('[module-installer] failed to load module:', modulePath, err)
    return null
  }
}

// Scan ~/.agent24/modules/node_modules/ for previously installed modules.
export function discoverInstalledModules(): { packageName: string; modulePath: string }[] {
  const nmDir = path.join(MODULES_DIR, 'node_modules')
  if (!fs.existsSync(nmDir)) return []

  const result: { packageName: string; modulePath: string }[] = []

  try {
    const entries = fs.readdirSync(nmDir)
    for (const entry of entries) {
      if (entry.startsWith('.')) continue

      if (entry.startsWith('@')) {
        // Scoped packages: @scope/name
        const scopeDir = path.join(nmDir, entry)
        const scoped = fs.readdirSync(scopeDir)
        for (const pkg of scoped) {
          result.push({
            packageName: `${entry}/${pkg}`,
            modulePath: path.join(scopeDir, pkg),
          })
        }
      } else {
        result.push({ packageName: entry, modulePath: path.join(nmDir, entry) })
      }
    }
  } catch { /* best-effort */ }

  return result
}
