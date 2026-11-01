import { execFile } from 'node:child_process'
import fs from 'node:fs'
import fsp from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import { promisify } from 'node:util'

import { checkOpenDesignSource, writeOpenDesignSourceStamp } from './open-design-mac-release.mjs'

const execFileAsync = promisify(execFile)

export const OPEN_DESIGN_SHA = '34bc2ba810cf894daff6a7c1736d1cef6c7726b1'
export const OPEN_DESIGN_PIN_VERSION = '0.22.2'

function requireAbsolutePath(value, label) {
  if (typeof value !== 'string' || value.length === 0 || !path.isAbsolute(value)) {
    throw new Error(`${label} must be an absolute path`)
  }
  return fs.realpathSync(value)
}

async function requireFile(filePath) {
  const stat = await fsp.stat(filePath).catch(() => null)
  if (stat == null || !stat.isFile()) throw new Error(`required file is missing: ${filePath}`)
}

async function requireDirectory(directoryPath) {
  const stat = await fsp.stat(directoryPath).catch(() => null)
  if (stat == null || !stat.isDirectory()) throw new Error(`required directory is missing: ${directoryPath}`)
}

async function run(command, args, options = {}) {
  const result = await execFileAsync(command, args, {
    cwd: options.cwd,
    env: options.env ?? process.env,
    maxBuffer: 32 * 1024 * 1024,
  })
  return result.stdout.trim()
}

async function runPnpm(openDesignRoot, args, extraEnv = {}) {
  await run('pnpm', args, {
    cwd: openDesignRoot,
    env: { ...process.env, ...extraEnv },
  })
}

async function loadReviewedPrebundleContract(openDesignRoot) {
  const script = [
    "import { WIN_DAEMON_PREBUNDLE_ESM_REQUIRE_BANNER, WIN_PREBUNDLE_ESBUILD_TARGET, WIN_PREBUNDLE_POLICIES } from './tools/pack/src/win/prebundle.ts'",
    'process.stdout.write(JSON.stringify({ banner: WIN_DAEMON_PREBUNDLE_ESM_REQUIRE_BANNER, policies: WIN_PREBUNDLE_POLICIES, target: WIN_PREBUNDLE_ESBUILD_TARGET }))',
  ].join('; ')
  const raw = await run('pnpm', ['exec', 'tsx', '-e', script], { cwd: openDesignRoot })
  const contract = JSON.parse(raw)
  if (contract.target !== 'node24') throw new Error(`unexpected Open Design prebundle target: ${contract.target}`)
  return contract
}

function assertMetafile(metaPath, policy) {
  const meta = JSON.parse(fs.readFileSync(metaPath, 'utf8'))
  const inputs = Object.keys(meta.inputs ?? {}).map((value) => value.replaceAll('\\', '/'))
  const forbidden = inputs.filter((input) =>
    policy.forbiddenInputs.some((needle) => input.includes(needle)),
  )
  if (forbidden.length > 0) {
    throw new Error(`${policy.label} prebundle included forbidden inputs: ${forbidden.join(', ')}`)
  }
}

function relativeImport(fromDirectory, targetPath) {
  const specifier = path.relative(fromDirectory, targetPath).replaceAll('\\', '/')
  return specifier.startsWith('.') ? specifier : `./${specifier}`
}

function isWithin(parent, child) {
  const relative = path.relative(parent, child)
  return relative.length === 0 || (!relative.startsWith('..') && !path.isAbsolute(relative))
}

/** Parents of a hoist link, from `webNodeModules` down through each scope
 * directory. Existing ones must realpath inside the standalone root. A
 * symlink whose own target resolves outside is rejected even before mkdir. */
async function assertHoistWriteInside(standaloneRoot, webNodeModules, linkPath) {
  const parent = path.dirname(linkPath)
  const relative = path.relative(webNodeModules, parent)
  if (relative.startsWith('..') || path.isAbsolute(relative)) {
    throw new Error(`hoist refuses to write ${linkPath}: it is outside apps/web/node_modules`)
  }
  const dirs = [webNodeModules]
  if (relative && relative !== '.') {
    let cursor = webNodeModules
    for (const part of relative.split(path.sep)) {
      cursor = path.join(cursor, part)
      dirs.push(cursor)
    }
  }
  for (const dir of dirs) {
    const st = await fsp.lstat(dir).catch(() => null)
    if (st == null) continue
    if (st.isSymbolicLink()) {
      const text = await fsp.readlink(dir)
      const direct = path.resolve(path.dirname(dir), text)
      const directReal = await fsp.realpath(direct).catch(() => null)
      if (directReal == null || !isWithin(standaloneRoot, directReal)) {
        throw new Error(`hoist refuses to write through symlink ${dir}: it points outside the standalone root (${directReal ?? direct})`)
      }
    }
    const real = await fsp.realpath(dir).catch(() => null)
    if (real == null || !isWithin(standaloneRoot, real)) {
      throw new Error(`hoist refuses to write through ${dir}: realpath is outside the standalone root (${real ?? dir})`)
    }
  }
}

async function rewriteStandaloneSymlinks({ destinationRoot, destinationWebRoot, sourceRoot, sourceWebRoot }) {
  const mappings = [
    [path.join(destinationWebRoot, 'node_modules'), path.join(sourceWebRoot, 'node_modules')],
    [path.join(destinationRoot, 'node_modules'), path.join(sourceRoot, 'node_modules')],
    [destinationWebRoot, sourceWebRoot],
    [destinationRoot, sourceRoot],
  ]

  function mapPath(value, fromIndex, toIndex) {
    for (const mapping of mappings) {
      if (isWithin(mapping[fromIndex], value)) {
        return path.join(mapping[toIndex], path.relative(mapping[fromIndex], value))
      }
    }
    return null
  }

  async function visit(current) {
    const metadata = await fsp.lstat(current).catch(() => null)
    if (metadata == null) return
    if (metadata.isSymbolicLink()) {
      const sourcePath = mapPath(current, 0, 1)
      if (sourcePath == null) return
      const currentTarget = await fsp.readlink(current)
      const sourceTarget = path.resolve(path.dirname(sourcePath), currentTarget)
      const destinationTarget = mapPath(sourceTarget, 1, 0)
      if (destinationTarget == null) return
      const nextTarget = path.relative(path.dirname(current), destinationTarget) || '.'
      if (nextTarget !== currentTarget) {
        await fsp.rm(current, { force: true, recursive: true })
        await fsp.symlink(nextTarget, current)
      }
      return
    }
    if (!metadata.isDirectory()) return
    for (const entry of await fsp.readdir(current, { withFileTypes: true })) {
      await visit(path.join(current, entry.name))
    }
  }

  await visit(destinationRoot)
}

async function pruneBrokenSymlinks(current) {
  const metadata = await fsp.lstat(current).catch(() => null)
  if (metadata == null) return
  if (metadata.isSymbolicLink()) {
    if (!fs.existsSync(current)) await fsp.rm(current, { force: true })
    return
  }
  if (!metadata.isDirectory()) return
  for (const entry of await fsp.readdir(current, { withFileTypes: true })) {
    await pruneBrokenSymlinks(path.join(current, entry.name))
  }
}

/**
 * Makes the pnpm-layout Next standalone tree resolvable WITHOUT realpath.
 *
 * `next build` (standalone) on a pnpm workspace emits `apps/web/node_modules/next`
 * as a symlink into `node_modules/.pnpm/<id>/node_modules/next`, whose own
 * dependencies (`@swc/helpers`, `styled-jsx`, `react`, ...) are SIBLINGS of that
 * real directory. That only resolves if the module loader realpath()s the
 * symlink first. Inside Open Design's web sidecar (Electron-as-Node, in-process
 * `import()` of server.js) the loader keeps the symlinked path, so
 * `require('@swc/helpers/_/...')` from next/dist/... walks
 * `<webRoot>/node_modules` and fails with MODULE_NOT_FOUND (the Design page
 * then reports a daemon that "exited before reporting status").
 *
 * Fix: also expose the transitive closure of those siblings under
 * `<webRoot>/node_modules` (what `shamefully-hoist` would do), as relative
 * symlinks that point straight at real directories.
 *
 * Darwin only (`options.platform`, default `process.platform`). Linux keeps
 * the pre-hoist layout: its loader realpaths symlinks, and running this
 * there would change which copy of a duplicated package wins.
 *
 * The same package name with two different realpaths fails the build and
 * prints every conflicting path. Exception: if `apps/web/package.json`
 * directly depends on that name and `apps/web/node_modules` already links
 * one realpath, that direct dependency is the one that is kept, and the
 * choice is printed. Every new link target's realpath must stay inside
 * `standaloneRoot`. So must the path we write: `apps/web/node_modules` and
 * every scope parent (`@scope`) have to realpath inside the tree, and must
 * not be a symlink that points outside — `mkdir` / `symlink` would follow
 * that link and create the package outside the standalone root.
 */
export async function hoistStandalonePnpmDependencies(standaloneRoot, webRoot = path.join(standaloneRoot, 'apps', 'web'), options = {}) {
  const platform = options.platform ?? process.platform
  if (platform !== 'darwin') return 0
  const log = options.log ?? ((line) => process.stderr.write(`${line}\n`))
  // Work on realpaths throughout: the relative links written below are computed
  // against realpath'd targets, so a symlinked prefix (macOS /var -> /private/var)
  // would otherwise yield links that climb out of the tree.
  if (!fs.existsSync(webRoot)) return 0
  standaloneRoot = await fsp.realpath(standaloneRoot)
  webRoot = await fsp.realpath(webRoot)
  const webNodeModules = path.join(webRoot, 'node_modules')
  const pnpmRoot = path.join(standaloneRoot, 'node_modules', '.pnpm')
  if (!fs.existsSync(webNodeModules) || !fs.existsSync(pnpmRoot)) return 0

  const packageRealDir = async (entryPath) => fsp.realpath(entryPath).catch(() => null)
  const listPackages = async (nodeModulesDir) => {
    const found = []
    for (const entry of await fsp.readdir(nodeModulesDir, { withFileTypes: true })) {
      if (entry.name === '.bin' || entry.name === '.pnpm') continue
      const full = path.join(nodeModulesDir, entry.name)
      if (entry.name.startsWith('@')) {
        for (const scoped of await fsp.readdir(full, { withFileTypes: true })) {
          found.push({ name: `${entry.name}/${scoped.name}`, full: path.join(full, scoped.name) })
        }
      } else {
        found.push({ name: entry.name, full })
      }
    }
    return found
  }

  const pnpmReal = await fsp.realpath(pnpmRoot)
  const directNames = await readWebDirectDependencyNames(webRoot)
  const initial = await listPackages(webNodeModules)
  const chosen = new Map()
  const directChosen = new Map()
  for (const pkg of initial) {
    const real = await packageRealDir(pkg.full)
    if (real == null) continue
    chosen.set(pkg.name, real)
    if (directNames.has(pkg.name)) directChosen.set(pkg.name, real)
  }
  const queue = initial.map((pkg) => pkg.full)
  const conflicts = new Map()
  let added = 0
  while (queue.length > 0) {
    const real = await packageRealDir(queue.pop())
    if (real == null || !isWithin(pnpmReal, real)) continue
    // <pnpm>/<id>/node_modules/<name | @scope/name>: the siblings live next to it.
    const containing = real.split(path.sep).lastIndexOf('node_modules')
    if (containing < 0) continue
    const siblingsDir = real.split(path.sep).slice(0, containing + 1).join(path.sep)
    for (const sibling of await listPackages(siblingsDir)) {
      const target = await packageRealDir(sibling.full)
      if (target == null) continue
      if (!isWithin(standaloneRoot, target)) {
        throw new Error(`hoist refuses ${sibling.name}: link target realpath is outside the standalone root (${target})`)
      }
      const existing = chosen.get(sibling.name)
      if (existing == null) {
        const linkPath = path.join(webNodeModules, ...sibling.name.split('/'))
        await assertHoistWriteInside(standaloneRoot, webNodeModules, linkPath)
        chosen.set(sibling.name, target)
        await fsp.mkdir(path.dirname(linkPath), { recursive: true })
        await fsp.symlink(path.relative(path.dirname(linkPath), target), linkPath)
        queue.push(linkPath)
        added += 1
        continue
      }
      if (existing === target) continue
      if (directChosen.has(sibling.name)) {
        log(`hoist: apps/web direct dependency selects ${sibling.name} (${await describeHoistedPackage(directChosen.get(sibling.name))}); skipping ${await describeHoistedPackage(target)}`)
        continue
      }
      if (!conflicts.has(sibling.name)) conflicts.set(sibling.name, new Set([existing]))
      conflicts.get(sibling.name).add(target)
    }
  }
  if (conflicts.size > 0) {
    const lines = []
    for (const [name, paths] of conflicts) {
      lines.push(`  ${name}:`)
      for (const item of paths) lines.push(`    - ${await describeHoistedPackage(item)}`)
    }
    const message = `hoist conflict: same package name, different realpaths, and apps/web has no direct dependency to choose:\n${lines.join('\n')}`
    log(message)
    throw new Error(message)
  }
  return added
}

async function readWebDirectDependencyNames(webRoot) {
  let pkg
  try {
    pkg = JSON.parse(await fsp.readFile(path.join(webRoot, 'package.json'), 'utf8'))
  } catch {
    return new Set()
  }
  return new Set([
    ...Object.keys(pkg.dependencies ?? {}),
    ...Object.keys(pkg.optionalDependencies ?? {}),
    ...Object.keys(pkg.peerDependencies ?? {}),
  ])
}

async function describeHoistedPackage(dir) {
  try {
    const pkg = JSON.parse(await fsp.readFile(path.join(dir, 'package.json'), 'utf8'))
    return pkg.version ? `${pkg.version} ${dir}` : dir
  } catch {
    return dir
  }
}

async function buildStandaloneWorkspace(openDesignRoot) {
  const env = { OD_WEB_OUTPUT_MODE: 'standalone' }
  await runPnpm(openDesignRoot, [
    '--filter', '@open-design/packaged^...',
    '--workspace-concurrency=1', '--if-present', 'run', 'build',
  ], env)
  await runPnpm(openDesignRoot, ['--filter', '@open-design/web', 'run', 'build:sidecar'])
  await runPnpm(openDesignRoot, ['--filter', '@open-design/packaged', 'run', 'build'])
}

async function buildReviewedPrebundles(openDesignRoot, resourcesRoot) {
  const { banner, policies, target } = await loadReviewedPrebundleContract(openDesignRoot)
  const prebundledRoot = path.join(resourcesRoot, 'app', 'prebundled')
  const daemonRoot = path.join(prebundledRoot, 'daemon')
  // realpath: on macOS os.tmpdir() is /var/folders/... but /var is a symlink to
  // /private/var, so a relative import computed from the unresolved path
  // climbs the wrong number of levels once esbuild resolves it physically.
  const tempRoot = await fsp.realpath(await fsp.mkdtemp(path.join(os.tmpdir(), 'agent24-od-prebundle-')))
  const webMeta = path.join(tempRoot, 'web-sidecar.meta.json')
  const daemonMeta = path.join(tempRoot, 'daemon.meta.json')

  try {
    await fsp.rm(path.join(prebundledRoot, 'web-sidecar.mjs'), { force: true })
    await fsp.rm(daemonRoot, { force: true, recursive: true })
    await fsp.mkdir(daemonRoot, { recursive: true })

    await runPnpm(openDesignRoot, [
      '--filter', '@open-design/packaged', 'exec', 'esbuild',
      path.join(openDesignRoot, 'apps', 'web', 'dist', 'sidecar', 'index.js'),
      '--bundle', '--platform=node', '--format=esm', `--target=${target}`,
      ...policies.webSidecar.externals.map((dependency) => `--external:${dependency}`),
      `--outfile=${path.join(prebundledRoot, 'web-sidecar.mjs')}`,
      `--metafile=${webMeta}`,
    ])
    assertMetafile(webMeta, policies.webSidecar)

    const sidecarEntry = path.join(tempRoot, 'daemon-sidecar.js')
    const cliEntry = path.join(tempRoot, 'daemon-cli.js')
    await fsp.writeFile(
      sidecarEntry,
      `import ${JSON.stringify(relativeImport(tempRoot, path.join(openDesignRoot, 'apps', 'daemon', 'dist', 'sidecar', 'index.js')))};\n`,
      'utf8',
    )
    await fsp.writeFile(
      cliEntry,
      [
        'import { fileURLToPath } from "node:url";',
        'const selfPath = fileURLToPath(import.meta.url);',
        'process.env.OD_BIN ??= selfPath;',
        'process.env.OD_DAEMON_CLI_PATH ??= selfPath;',
        `await import(${JSON.stringify(relativeImport(tempRoot, path.join(openDesignRoot, 'apps', 'daemon', 'dist', 'cli.js')))});`,
        '',
      ].join('\n'),
      'utf8',
    )
    await runPnpm(openDesignRoot, [
      '--filter', '@open-design/packaged', 'exec', 'esbuild',
      sidecarEntry, cliEntry,
      '--bundle', '--splitting', '--platform=node', '--format=esm', `--target=${target}`,
      `--banner:js=${banner}`,
      ...policies.daemonSidecar.externals.map((dependency) => `--external:${dependency}`),
      // macOS only: chokidar's optional `fsevents` is installed on darwin (not on
      // Linux) and is a native .node esbuild cannot bundle. chokidar loads it in
      // try/catch and falls back to fs.watch, so keep it external.
      ...(process.platform === 'darwin' ? ['--external:fsevents'] : []),
      `--outdir=${daemonRoot}`, '--entry-names=[name]', '--chunk-names=chunks/[name]-[hash]',
      '--out-extension:.js=.mjs', `--metafile=${daemonMeta}`,
    ])
    assertMetafile(daemonMeta, policies.daemonSidecar)
    assertMetafile(daemonMeta, policies.daemonCli)
  } finally {
    await fsp.rm(tempRoot, { force: true, recursive: true })
  }
}

async function copyStandaloneWeb(openDesignRoot, resourcesRoot) {
  const sourceRoot = path.join(openDesignRoot, 'apps', 'web', '.next', 'standalone')
  const nestedServer = path.join(sourceRoot, 'apps', 'web', 'server.js')
  const rootServer = path.join(sourceRoot, 'server.js')
  const sourceWebRoot = fs.existsSync(nestedServer)
    ? path.join(sourceRoot, 'apps', 'web')
    : fs.existsSync(rootServer) ? sourceRoot : null
  if (sourceWebRoot == null) throw new Error(`Next.js standalone server output is missing under ${sourceRoot}`)

  const destinationRoot = path.join(resourcesRoot, 'open-design-web-standalone')
  const destinationWebRoot = sourceWebRoot === sourceRoot
    ? destinationRoot
    : path.join(destinationRoot, 'apps', 'web')
  await fsp.rm(destinationRoot, { force: true, recursive: true })
  await fsp.cp(sourceRoot, destinationRoot, { recursive: true, verbatimSymlinks: true })
  await rewriteStandaloneSymlinks({ destinationRoot, destinationWebRoot, sourceRoot, sourceWebRoot })
  await pruneBrokenSymlinks(destinationRoot)
  await hoistStandalonePnpmDependencies(destinationRoot, destinationWebRoot)

  const staticRoot = path.join(openDesignRoot, 'apps', 'web', '.next', 'static')
  const publicRoot = path.join(openDesignRoot, 'apps', 'web', 'public')
  await requireDirectory(staticRoot)
  await requireDirectory(publicRoot)
  await fsp.cp(staticRoot, path.join(destinationWebRoot, '.next', 'static'), {
    recursive: true,
    dereference: true,
    force: true,
  })
  await fsp.cp(publicRoot, path.join(destinationWebRoot, 'public'), {
    recursive: true,
    dereference: true,
    force: true,
  })
}

export async function prepareOpenDesignLinuxResources(openDesignPath, linuxResourcesPath) {
  // The assembly below is platform-neutral (esbuild prebundles + Next standalone
  // copy); the only platform-specific part is the tools-pack Resources base it
  // is layered on. macOS release builds (release-mac.sh) reuse it unchanged
  // on top of `tools-pack mac build --to app --portable` Resources.
  if (process.platform !== 'linux' && process.platform !== 'darwin') {
    throw new Error('Open Design resources must be prepared on Linux or macOS')
  }
  const openDesignRoot = requireAbsolutePath(openDesignPath, 'Open Design checkout')
  const resourcesRoot = requireAbsolutePath(linuxResourcesPath, 'Open Design Linux Resources')
  // Same check release-mac.sh and rebuild-open-design-native call: porcelain
  // empty and HEAD equal to the pin, before the build and before the stamp.
  const actualSha = checkOpenDesignSource(openDesignRoot)

  const packaged = JSON.parse(await fsp.readFile(path.join(openDesignRoot, 'apps', 'packaged', 'package.json'), 'utf8'))
  if (packaged.version !== OPEN_DESIGN_PIN_VERSION) {
    throw new Error(`Open Design packaged version must be ${OPEN_DESIGN_PIN_VERSION}; found ${packaged.version}`)
  }
  const packagedResource = JSON.parse(await fsp.readFile(path.join(resourcesRoot, 'app', 'package.json'), 'utf8'))
  if (packagedResource.version !== OPEN_DESIGN_PIN_VERSION) {
    throw new Error(`Linux tools-pack Resources version must be ${OPEN_DESIGN_PIN_VERSION}; found ${packagedResource.version}`)
  }

  await Promise.all([
    requireDirectory(path.join(resourcesRoot, 'open-design')),
    requireDirectory(path.join(resourcesRoot, 'app', 'node_modules')),
    requireFile(path.join(resourcesRoot, 'app', 'prebundled', 'agent24-headless.cjs')),
    requireFile(path.join(resourcesRoot, 'app', 'prebundled', 'agent24-headless.mjs')),
    requireFile(path.join(resourcesRoot, 'app', 'node_modules', '@open-design', 'sidecar', 'dist', 'index.mjs')),
  ])

  await buildStandaloneWorkspace(openDesignRoot)
  await buildReviewedPrebundles(openDesignRoot, resourcesRoot)
  await copyStandaloneWeb(openDesignRoot, resourcesRoot)
  await writeOpenDesignSourceStamp(resourcesRoot, { sha: actualSha, version: packaged.version })
  return resourcesRoot
}

if (process.argv[1] != null && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [openDesignPath, linuxResourcesPath] = process.argv.slice(2)
  if (openDesignPath == null || linuxResourcesPath == null || process.argv.length !== 4) {
    throw new Error('usage: prepare-open-design-linux-resources.mjs <absolute-open-design-checkout> <absolute-linux-resources>')
  }
  process.stdout.write(`${await prepareOpenDesignLinuxResources(openDesignPath, linuxResourcesPath)}\n`)
}
