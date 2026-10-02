import { execFile } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { promisify } from 'node:util'

const execFileAsync = promisify(execFile)
const scriptDir = path.dirname(fileURLToPath(import.meta.url))
const desktopRoot = path.resolve(scriptDir, '..')
const stagedAppRoot = path.join(desktopRoot, '.open-design-resources', 'app')

function requireFile(filePath, label) {
  const stat = fs.statSync(filePath, { throwIfNoEntry: false })
  if (!stat?.isFile()) throw new Error(`${label} is missing: ${filePath}`)
}

function resolveElectronRuntime() {
  const manifestPath = path.join(desktopRoot, 'node_modules', 'electron', 'package.json')
  requireFile(manifestPath, 'Agent24 Electron manifest')
  const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'))
  const packageRoot = path.dirname(manifestPath)
  const pathFile = path.join(packageRoot, 'path.txt')
  let executable = null
  if (fs.statSync(pathFile, { throwIfNoEntry: false })?.isFile()) {
    const relativeExecutable = fs.readFileSync(pathFile, 'utf8').trim()
    if (relativeExecutable.length > 0) {
      const candidate = path.resolve(packageRoot, 'dist', relativeExecutable)
      if (fs.statSync(candidate, { throwIfNoEntry: false })?.isFile()) executable = candidate
    }
  }
  return { executable, version: manifest.version }
}

async function run(command, args, options = {}) {
  return execFileAsync(command, args, {
    cwd: options.cwd,
    env: options.env ?? process.env,
    maxBuffer: 32 * 1024 * 1024,
  })
}

export async function rebuildOpenDesignNativeModules() {
  const packagePath = path.join(stagedAppRoot, 'package.json')
  const sqliteManifest = path.join(stagedAppRoot, 'node_modules', 'better-sqlite3', 'package.json')
  requireFile(packagePath, 'staged Open Design package manifest')
  requireFile(sqliteManifest, 'staged Open Design better-sqlite3 manifest')
  const sqliteRoot = path.dirname(sqliteManifest)
  const sqlitePackage = JSON.parse(fs.readFileSync(sqliteManifest, 'utf8'))
  if (sqlitePackage.name !== 'better-sqlite3') {
    throw new Error('staged Open Design better-sqlite3 package identity changed')
  }
  const buildRelease = sqlitePackage.scripts?.['build-release']
  const hasPackagedScripts = sqlitePackage.scripts != null
  if (hasPackagedScripts && buildRelease !== 'node-gyp rebuild --release') {
    throw new Error('staged Open Design better-sqlite3 build-release contract changed')
  }
  if (!hasPackagedScripts) {
    requireFile(path.join(sqliteRoot, 'binding.gyp'), 'staged Open Design better-sqlite3 binding.gyp')
  }

  const { executable, version } = resolveElectronRuntime()
  const arch = process.env.A24_ELECTRON_ARCH?.trim() || process.arch
  const platform = process.env.A24_ELECTRON_PLATFORM?.trim() || process.platform
  const rebuildEnv = {
    ...process.env,
    npm_config_runtime: 'electron',
    npm_config_target: version,
    npm_config_arch: arch,
    npm_config_target_arch: arch,
    npm_config_platform: platform,
    npm_config_disturl: 'https://electronjs.org/headers',
    npm_config_build_from_source: 'true',
  }

  const npmArgs = hasPackagedScripts ? ['run', 'build-release'] : ['rebuild', '--foreground-scripts']
  if (process.platform === 'win32') {
    await run('cmd.exe', ['/d', '/s', '/c', 'npm.cmd', ...npmArgs], { cwd: sqliteRoot, env: rebuildEnv })
  } else {
    await run('npm', npmArgs, { cwd: sqliteRoot, env: rebuildEnv })
  }

  const probe = [
    "const Database = require('better-sqlite3')",
    "const db = new Database(':memory:')",
    'db.close()',
  ].join('; ')
  const canProbeHostRuntime = platform === process.platform && arch === process.arch
  if (executable != null && canProbeHostRuntime) {
    await run(executable, ['-e', probe], {
      cwd: stagedAppRoot,
      env: { ...process.env, ELECTRON_RUN_AS_NODE: '1' },
    })
  }

  process.stdout.write(
    `Rebuilt Open Design native modules for Electron ${version} (${platform}-${arch}); ` +
      `runtime probe ${executable == null || !canProbeHostRuntime ? 'deferred to packaged validation' : 'passed'}\n`,
  )
}

if (process.argv[1] != null && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await rebuildOpenDesignNativeModules()
}
