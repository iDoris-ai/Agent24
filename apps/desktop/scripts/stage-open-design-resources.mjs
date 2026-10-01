import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const OPEN_DESIGN_PIN_VERSION = '0.22.2'
const RESOURCE_TREES = ['app', 'open-design', 'open-design-web-standalone']
const scriptDir = path.dirname(fileURLToPath(import.meta.url))
const stageRoot = path.resolve(scriptDir, '../.open-design-resources')
const rawSource = process.env.A24_OPEN_DESIGN_RESOURCES_DIR?.trim()

function resetStage() {
  fs.rmSync(stageRoot, { recursive: true, force: true })
  fs.mkdirSync(stageRoot, { recursive: true })
}

function requireFile(root, relative) {
  const target = path.join(root, relative)
  const stat = fs.statSync(target, { throwIfNoEntry: false })
  if (!stat?.isFile()) throw new Error(`Open Design packaged resource is missing: ${relative}`)
  return target
}

function requireDirectory(root, relative) {
  const target = path.join(root, relative)
  const stat = fs.statSync(target, { throwIfNoEntry: false })
  if (!stat?.isDirectory()) throw new Error(`Open Design packaged resource directory is missing: ${relative}`)
  return target
}

function verifyVersion(sourceRoot) {
  const packagePath = requireFile(sourceRoot, 'app/package.json')
  const manifest = JSON.parse(fs.readFileSync(packagePath, 'utf8'))
  if (manifest?.version !== OPEN_DESIGN_PIN_VERSION) {
    throw new Error(
      `Open Design packaged resource version mismatch: expected ${OPEN_DESIGN_PIN_VERSION}, got ${String(manifest?.version)}`,
    )
  }
}

function verifyContract(sourceRoot) {
  verifyVersion(sourceRoot)
  for (const relative of [
    'app/prebundled/agent24-headless.cjs',
    'app/prebundled/agent24-headless.mjs',
    'app/prebundled/daemon/daemon-cli.mjs',
    'app/prebundled/daemon/daemon-sidecar.mjs',
    'app/prebundled/web-sidecar.mjs',
  ]) requireFile(sourceRoot, relative)
  requireDirectory(sourceRoot, 'open-design')
  requireDirectory(sourceRoot, 'open-design-web-standalone')
}

function isInsideResourceTrees(sourceRoot, target) {
  return RESOURCE_TREES.some((tree) => {
    const root = path.join(sourceRoot, tree)
    return target === root || target.startsWith(`${root}${path.sep}`)
  })
}

function verifySymlinks(sourceRoot, directory) {
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const fullPath = path.join(directory, entry.name)
    if (entry.isSymbolicLink()) {
      const link = fs.readlinkSync(fullPath)
      if (path.isAbsolute(link)) throw new Error(`Open Design packaged resource has an absolute symlink: ${fullPath}`)
      const resolved = fs.realpathSync(fullPath)
      if (!isInsideResourceTrees(sourceRoot, resolved)) {
        throw new Error(`Open Design packaged resource symlink escapes staged trees: ${fullPath}`)
      }
      continue
    }
    if (entry.isDirectory()) verifySymlinks(sourceRoot, fullPath)
  }
}

resetStage()

if (!rawSource) {
  throw new Error('A24_OPEN_DESIGN_RESOURCES_DIR is required for packaged Agent24 Creative builds')
}

if (!path.isAbsolute(rawSource)) {
  throw new Error('A24_OPEN_DESIGN_RESOURCES_DIR must be an absolute Open Design Resources path')
}

const sourceRoot = fs.realpathSync(rawSource)
verifyContract(sourceRoot)
for (const relative of RESOURCE_TREES) verifySymlinks(sourceRoot, path.join(sourceRoot, relative))

for (const relative of RESOURCE_TREES) {
  fs.cpSync(path.join(sourceRoot, relative), path.join(stageRoot, relative), {
    recursive: true,
    force: false,
    errorOnExist: true,
    verbatimSymlinks: true,
  })
}

process.stdout.write(`Staged Open Design ${OPEN_DESIGN_PIN_VERSION} resources from ${sourceRoot}\n`)
