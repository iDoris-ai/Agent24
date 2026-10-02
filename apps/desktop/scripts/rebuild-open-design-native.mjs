import { execFile } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { promisify } from 'node:util'

const execFileAsync = promisify(execFile)
const OPEN_DESIGN_SHA = '327de2887fcd8d6f71a3598e921c0f12521d37d7'
const BETTER_SQLITE3_VERSION = '12.10.0'
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

async function resolveNativeSource() {
  const sourceRootValue = process.env.A24_OPEN_DESIGN_SOURCE_DIR?.trim()
  if (!sourceRootValue || !path.isAbsolute(sourceRootValue)) {
    throw new Error('A24_OPEN_DESIGN_SOURCE_DIR must be an absolute exact Open Design checkout path')
  }
  const sourceRoot = fs.realpathSync(sourceRootValue)
  const { stdout: sourceSha } = await run('git', ['rev-parse', 'HEAD'], { cwd: sourceRoot })
  if (sourceSha.trim() !== OPEN_DESIGN_SHA) {
    throw new Error(`Open Design native source must be exact ${OPEN_DESIGN_SHA}`)
  }

  const pnpmRoot = path.join(sourceRoot, 'node_modules', '.pnpm')
  const packageRoot = path.join(
    pnpmRoot,
    `better-sqlite3@${BETTER_SQLITE3_VERSION}`,
    'node_modules',
    'better-sqlite3',
  )
  const packagePath = path.join(packageRoot, 'package.json')
  requireFile(packagePath, 'Open Design source better-sqlite3 manifest')
  requireFile(path.join(packageRoot, 'binding.gyp'), 'Open Design source better-sqlite3 binding.gyp')
  const manifest = JSON.parse(fs.readFileSync(packagePath, 'utf8'))
  if (manifest.name !== 'better-sqlite3' || manifest.version !== BETTER_SQLITE3_VERSION) {
    throw new Error('Open Design source better-sqlite3 identity changed')
  }
  if (manifest.scripts?.['build-release'] !== 'node-gyp rebuild --release') {
    throw new Error('Open Design source better-sqlite3 build-release contract changed')
  }
  if (manifest.scripts?.['prebuild-release'] != null || manifest.scripts?.['postbuild-release'] != null) {
    throw new Error('Open Design source better-sqlite3 build-release lifecycle changed')
  }
  return packageRoot
}

export async function rebuildOpenDesignNativeModules() {
  const packagePath = path.join(stagedAppRoot, 'package.json')
  const sqliteManifest = path.join(stagedAppRoot, 'node_modules', 'better-sqlite3', 'package.json')
  requireFile(packagePath, 'staged Open Design package manifest')
  requireFile(sqliteManifest, 'staged Open Design better-sqlite3 manifest')
  const sqliteRoot = path.dirname(sqliteManifest)
  const sqlitePackage = JSON.parse(fs.readFileSync(sqliteManifest, 'utf8'))
  if (sqlitePackage.name !== 'better-sqlite3' || sqlitePackage.version !== BETTER_SQLITE3_VERSION) {
    throw new Error('staged Open Design better-sqlite3 package identity changed')
  }
  const nativeSource = await resolveNativeSource()

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

  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'agent24-od-native-'))
  const buildSource = path.join(tempRoot, 'better-sqlite3')
  try {
    fs.cpSync(nativeSource, buildSource, { recursive: true, dereference: true })
    fs.rmSync(path.join(buildSource, 'build'), { recursive: true, force: true })
    if (process.platform === 'win32') {
      await run('cmd.exe', ['/d', '/s', '/c', 'npm.cmd', 'run', 'build-release'], {
        cwd: buildSource,
        env: rebuildEnv,
      })
    } else {
      await run('npm', ['run', 'build-release'], { cwd: buildSource, env: rebuildEnv })
    }

    const builtModule = path.join(buildSource, 'build', 'Release', 'better_sqlite3.node')
    requireFile(builtModule, 'rebuilt Open Design better-sqlite3 native module')
    const stagedModule = path.join(sqliteRoot, 'build', 'Release', 'better_sqlite3.node')
    fs.mkdirSync(path.dirname(stagedModule), { recursive: true })
    fs.copyFileSync(builtModule, stagedModule)
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true })
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
