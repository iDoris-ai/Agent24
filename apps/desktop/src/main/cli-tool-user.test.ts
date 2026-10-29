import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { promisify } from 'node:util'
import { lstat, mkdir, mkdtemp, readFile, readdir, readlink, realpath, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { buildTrayView } from './tray-menu'

const { exec } = vi.hoisted(() => ({ exec: vi.fn() }))
vi.mock('node:child_process', () => ({
  execFile: Object.assign(vi.fn(), { [Symbol.for('nodejs.util.promisify.custom')]: exec }),
}))
vi.mock('node:fs/promises', async (load) => {
  const fs = await load<typeof import('node:fs/promises')>()
  return { ...fs, lstat: (file: string, ...args: []) => file === '/usr/local/bin/agent24'
    ? Promise.reject(Object.assign(new Error('missing'), { code: 'ENOENT' })) : fs.lstat(file, ...args) }
})
import { cliToolStatus, installBundledCli, installUserCli, inspectCliPath } from './cli-tool'
const childProcess = await vi.importActual<typeof import('node:child_process')>('node:child_process')
const run = promisify(childProcess.execFile)
let root = ''
let home = ''
let target = ''
let executable = ''
let resources = ''
let destination = ''
beforeEach(async () => {
  root = await realpath(await mkdtemp(path.join(tmpdir(), 'a24-user-cli-')))
  home = path.join(root, 'home')
  await mkdir(home)
  vi.stubEnv('HOME', home)
  executable = path.join(root, 'Agent24.app/Contents/MacOS/Agent24')
  resources = path.join(root, 'Agent24.app/Contents/Resources')
  target = path.join(resources, 'backend/agent24')
  destination = path.join(home, '.agent24/bin/agent24')
  await mkdir(path.dirname(executable), { recursive: true })
  await mkdir(path.dirname(target), { recursive: true })
  await writeFile(executable, '')
  await writeFile(target, '#!/bin/sh\necho "agent24 0.6.0"\n', { mode: 0o755 })
  exec.mockReset()
  exec.mockImplementation((file, args, options) => args[0] === '-ilc'
    ? Promise.resolve({ stdout: `\nAGENT24_PATH=${path.dirname(target)}`, stderr: '' }) : run(file, args, options))
})
afterEach(async () => { vi.unstubAllEnvs(); await rm(root, { recursive: true, force: true }) })
async function oldFile(content = '#!/bin/sh\necho "agent24 0.4.0"\n') {
  await mkdir(path.dirname(destination), { recursive: true })
  await writeFile(destination, content, { mode: 0o755 })
}
it('creates the missing user CLI link with 0755 directories', async () => {
  const mask = process.umask(0o077)
  try { expect(await installUserCli(target)).toMatchObject({ state: 'installed' }) }
  finally { process.umask(mask) }
  expect(await readlink(destination)).toBe(target)
  for (const dir of [path.dirname(destination), path.dirname(path.dirname(destination))]) {
    expect((await lstat(dir)).mode & 0o777).toBe(0o755)
  }
})
it.each(['0.6.0', '0.7.0'])('preserves a regular CLI at version %s', async (version) => {
  await oldFile(`#!/bin/sh\necho "agent24 ${version}"\n`)
  const original = await readFile(destination, 'utf8')
  expect(await installUserCli(target)).toEqual({ state: 'installed', backup: null })
  expect(await readFile(destination, 'utf8')).toBe(original)
  expect((await lstat(destination)).isFile()).toBe(true)
})
it('detects missing, current, foreign and dangling links separately', async () => {
  expect(await inspectCliPath(destination, target, '0.6.0')).toMatchObject({ link: 'missing', state: 'missing', version: null })
  await mkdir(path.dirname(destination), { recursive: true })
  await symlink(target, destination)
  expect(await inspectCliPath(destination, target, '0.6.0')).toMatchObject({ link: 'current', state: 'current', version: '0.6.0' })
  await rm(destination)
  await oldFile()
  const foreign = path.join(root, 'foreign-cli')
  await writeFile(foreign, await readFile(destination), { mode: 0o755 })
  await rm(destination)
  await symlink(foreign, destination)
  expect(await inspectCliPath(destination, target, '0.6.0')).toMatchObject({ link: 'other', state: 'outdated', version: '0.4.0' })
  await rm(foreign)
  expect(await inspectCliPath(destination, target, '0.6.0')).toMatchObject({ link: 'other', state: 'outdated', version: null })
  expect(await installUserCli(target)).toEqual({ state: 'installed', backup: null })
  expect(await readlink(destination)).toBe(target)
})
it.skipIf(process.platform !== 'darwin')('user-only repair never invokes administrator authorization or the system installer', async () => {
  const confirm = vi.fn(async () => false)
  expect(await installBundledCli({ executable, resources, packaged: true, userOnly: true, confirm })).toEqual({ state: 'installed', backup: null })
  expect(await readlink(destination)).toBe(target)
  expect(confirm).not.toHaveBeenCalled()
  expect(exec.mock.calls.some((call) => call[0] === '/usr/bin/osascript' || call[1].join(' ').includes('/usr/local/bin/agent24'))).toBe(false)
})
it.skipIf(process.platform !== 'darwin')('startup detection of missing CLI paths creates no directories', async () => {
  expect(await cliToolStatus(executable, resources, true)).toMatchObject({ state: 'missing' })
  expect(await readdir(home)).toEqual([])
})
it('backs up the old regular file before linking to the bundled CLI', async () => {
  await oldFile()
  const original = await readFile(destination, 'utf8')
  const result = await installUserCli(target)
  expect(result.state).toBe('installed')
  if (result.state !== 'installed' || !result.backup) throw new Error('missing backup')
  expect(result.backup).toMatch(/agent24\.bak-/)
  expect(await readFile(result.backup, 'utf8')).toBe(original)
  expect(await readlink(destination)).toBe(target)
})
it('replaces a foreign symlink without backing it up or modifying its target', async () => {
  const foreign = path.join(root, 'foreign')
  await writeFile(foreign, 'foreign CLI')
  await mkdir(path.dirname(destination), { recursive: true })
  await symlink(foreign, destination)
  expect(await installUserCli(target)).toEqual({ state: 'installed', backup: null })
  expect(await readlink(destination)).toBe(target)
  expect(await readFile(foreign, 'utf8')).toBe('foreign CLI')
  expect(await readdir(path.dirname(destination))).toEqual(['agent24'])
})
it.each(['#!/bin/sh\nexec /bin/sleep 30\n', '#!/bin/sh\nexit 1\n', '#!/bin/sh\necho nonsense\n'])('marks an unreadable version as outdated: %s', async (script) => {
  await oldFile(script)
  expect(await inspectCliPath(destination, target, '0.6.0')).toMatchObject({ state: 'outdated', link: 'other', version: null })
}, 6000)
it.skipIf(process.platform !== 'darwin')('startup detection prompts for the stale user CLI without writing to HOME', async () => {
  await oldFile()
  const original = await readFile(destination, 'utf8')
  const status = await cliToolStatus(executable, resources, true)
  expect(status).toMatchObject({ state: 'outdated', version: '0.4.0', paths: [
    { path: '/usr/local/bin/agent24', link: 'missing', state: 'missing' },
    { path: destination, link: 'other', state: 'outdated', version: '0.4.0' },
  ] })
  expect(buildTrayView({ status: 'running', external: false, externalPid: null, foreignPid: null, backendKind: 'rust', cliTool: status }).items)
    .toContainEqual(expect.objectContaining({ action: 'cli-settings', label: expect.stringContaining('0.4.0') }))
  expect(await readFile(destination, 'utf8')).toBe(original)
  expect((await lstat(destination)).isFile()).toBe(true)
  expect(await readdir(path.dirname(destination))).toEqual(['agent24'])
  expect(exec.mock.calls.every((call) => call[1][0] === '--version' || call[1][0] === '-ilc')).toBe(true)
})
