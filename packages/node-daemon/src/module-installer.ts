// M3: Community module installer — runs npm install/uninstall in
// <data_dir>/modules and dynamically loads the installed package.

import { execFile } from 'node:child_process'
import fs from 'node:fs'
import { promisify } from 'node:util'
import { dataDir, joinLiteral } from '@agent24/data-dir'
import type { CapabilityModule } from './capabilities/base'
import { validateManifest } from './module-manifest'

const execFileAsync = promisify(execFile)

/** `<data_dir>/modules`. Resolved on each call (ADR-037). */
export function modulesDir(): string {
  return joinLiteral(dataDir(), 'modules')
}

/** joinLiteral, but `/` inside a scoped package name stays a separator.
 * Does not fold `..` in the data dir (unlike path.join). */
function under(dir: string, ...parts: string[]): string {
  return joinLiteral(dir, ...parts.flatMap((part) => part.split('/').filter((segment) => segment !== '')))
}

// Ensure <data_dir>/modules exists with a package.json so npm install works.
function ensureModulesDir(): string {
  const dir = modulesDir()
  fs.mkdirSync(dir, { recursive: true })
  const pkg = joinLiteral(dir, 'package.json')
  if (!fs.existsSync(pkg)) {
    fs.writeFileSync(pkg, JSON.stringify({
      name: 'agent24-community-modules',
      version: '1.0.0',
      private: true,
      description: 'Agent24 community capability modules',
      dependencies: {},
    }, null, 2))
  }
  return dir
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
    const dir = ensureModulesDir()

    // Run npm install (use system npm; Electron bundles Node but not npm)
    await execFileAsync('npm', ['install', packageName, '--save'], {
      cwd: dir,
      timeout: 120_000,
      env: { ...process.env, NODE_ENV: 'production' },
    })

    // Resolve the installed package path
    const modulePath = under(dir, 'node_modules', packageName)

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
    const dir = ensureModulesDir()
    await execFileAsync('npm', ['uninstall', packageName, '--save'], {
      cwd: dir,
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
    // eslint-disable-next-line @typescript-eslint/no-require-imports, @typescript-eslint/no-var-requires
    const mod = require(modulePath) as { manifest?: unknown; register?: unknown }
    if (
      !mod ||
      typeof mod.manifest !== 'object' || mod.manifest === null ||
      typeof mod.register !== 'function'
    ) {
      return null
    }
    // E3: the manifest must conform to protocol/module.schema.json before the
    // module is allowed to register anything. A malformed manifest is rejected
    // here rather than surfacing later as an opaque runtime failure.
    const errors = validateManifest(mod.manifest)
    if (errors.length > 0) {
      console.error(
        `[module-installer] rejecting ${modulePath}: manifest failed schema validation: ${errors.join('; ')}`,
      )
      return null
    }
    return mod as CapabilityModule
  } catch (err) {
    console.error('[module-installer] failed to load module:', modulePath, err)
    return null
  }
}

// Scan <data_dir>/modules/node_modules for previously installed modules.
export function discoverInstalledModules(): { packageName: string; modulePath: string }[] {
  const dir = modulesDir()
  const nmDir = joinLiteral(dir, 'node_modules')
  if (!fs.existsSync(nmDir)) return []

  const result: { packageName: string; modulePath: string }[] = []

  try {
    const entries = fs.readdirSync(nmDir)
    for (const entry of entries) {
      if (entry.startsWith('.')) continue

      if (entry.startsWith('@')) {
        // Scoped packages: @scope/name
        const scopeDir = under(dir, 'node_modules', entry)
        const scoped = fs.readdirSync(scopeDir)
        for (const pkg of scoped) {
          result.push({
            packageName: `${entry}/${pkg}`,
            modulePath: under(dir, 'node_modules', entry, pkg),
          })
        }
      } else {
        result.push({ packageName: entry, modulePath: under(dir, 'node_modules', entry) })
      }
    }
  } catch { /* best-effort */ }

  return result
}
