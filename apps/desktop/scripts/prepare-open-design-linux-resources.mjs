import { execFile } from 'node:child_process'
import fs from 'node:fs'
import fsp from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import { promisify } from 'node:util'

const execFileAsync = promisify(execFile)

export const OPEN_DESIGN_SHA = 'e4be3d5d8ca0ce8c624b269b3b507b701788dae1'
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
  const tempRoot = await fsp.mkdtemp(path.join(os.tmpdir(), 'agent24-od-prebundle-'))
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
  if (process.platform !== 'linux') throw new Error('Open Design Linux resources must be prepared on Linux')
  const openDesignRoot = requireAbsolutePath(openDesignPath, 'Open Design checkout')
  const resourcesRoot = requireAbsolutePath(linuxResourcesPath, 'Open Design Linux Resources')
  const actualSha = await run('git', ['rev-parse', 'HEAD'], { cwd: openDesignRoot })
  if (actualSha !== OPEN_DESIGN_SHA) {
    throw new Error(`Open Design checkout must be exact ${OPEN_DESIGN_SHA}; found ${actualSha}`)
  }

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
  return resourcesRoot
}

if (process.argv[1] != null && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [openDesignPath, linuxResourcesPath] = process.argv.slice(2)
  if (openDesignPath == null || linuxResourcesPath == null || process.argv.length !== 4) {
    throw new Error('usage: prepare-open-design-linux-resources.mjs <absolute-open-design-checkout> <absolute-linux-resources>')
  }
  process.stdout.write(`${await prepareOpenDesignLinuxResources(openDesignPath, linuxResourcesPath)}\n`)
}
