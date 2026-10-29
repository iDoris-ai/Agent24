import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { promisify } from 'node:util'
import { chmod, mkdir, mkdtemp, readFile, realpath, rename, rm, symlink, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { tmpdir } from 'node:os'

const { exec } = vi.hoisted(() => ({ exec: vi.fn() }))
vi.mock('node:child_process', () => ({
  execFile: Object.assign(vi.fn(), { [Symbol.for('nodejs.util.promisify.custom')]: exec }),
}))
import { cliToolStatus, installBundledCli, installLink } from './cli-tool'

const childProcess = await vi.importActual<typeof import('node:child_process')>('node:child_process')
const run = promisify(childProcess.execFile)
let root = ''
beforeEach(() => { exec.mockReset(); exec.mockImplementation(run) })
afterEach(async () => {
  vi.unstubAllEnvs()
  if (root) await rm(root, { recursive: true, force: true })
})
async function fixture() {
  root = await realpath(await mkdtemp(path.join(tmpdir(), 'a24-exec-')))
  vi.stubEnv('HOME', path.join(root, 'home'))
  const executable = path.join(root, 'Agent24.app/Contents/MacOS/Agent24')
  const resources = path.join(root, 'Agent24.app/Contents/Resources')
  const target = path.join(resources, 'backend/agent24')
  await mkdir(path.dirname(executable), { recursive: true })
  await mkdir(path.dirname(target), { recursive: true })
  await writeFile(executable, '')
  await writeFile(target, '#!/bin/sh\necho "agent24 0.6.0"\n', { mode: 0o755 })
  await chmod(target, 0o755)
  return { executable, resources, target, destination: path.join(root, 'agent24') }
}

it.skipIf(process.platform !== 'darwin')('falls back to the process PATH when the login shell omits its marker', async () => {
  const f = await fixture()
  vi.stubEnv('PATH', path.dirname(f.target))
  exec.mockImplementation((file, args, options) => args[0] === '-ilc'
    ? Promise.resolve({ stdout: 'unrelated shell output', stderr: '' }) : run(file, args, options))
  expect(await cliToolStatus(f.executable, f.resources, true)).toMatchObject({ bundledVersion: '0.6.0' })
  // Both the PATH command and the bundle resolve to this temporary executable.
  expect(exec.mock.calls.filter((call) => call[0] === f.target && call[1][0] === '--version')).toHaveLength(2)
})
it('restores the old command and reports shell failure when linking fails', async () => {
  const f = await fixture()
  await writeFile(f.destination, 'old CLI')
  exec.mockImplementation((file, args, options) => run(file, [args[0], args[1].replace('/bin/ln -s', '/usr/bin/false')], options))
  await expect(installLink({ ...f, confirm: async () => true })).rejects.toMatchObject({ code: 1 })
  expect(await readFile(f.destination, 'utf8')).toBe('old CLI')
})
it('rejects elevation for any destination other than the fixed CLI path', async () => {
  const f = await fixture()
  exec.mockRejectedValue(new Error('Permission denied'))
  const elevate = vi.fn(async () => {})
  await expect(installLink({ ...f, confirm: async () => true, elevate })).rejects.toThrow('Invalid privileged destination')
  expect(elevate).not.toHaveBeenCalled()
})
it('verifies the installed link before reporting success', async () => {
  const f = await fixture()
  exec.mockImplementation(async (file, args, options) => {
    const output = await run(file, args, options)
    await rm(f.destination)
    await symlink('/missing/other-cli', f.destination)
    return output
  })
  await expect(installLink({ ...f, confirm: async () => true })).rejects.toThrow('Installed link verification failed')
})
it.skipIf(process.platform !== 'darwin').each(['Destination changed', 'Permission denied'])('returns only a safe failure result for %s with private diagnostics', async (reason) => {
  const f = await fixture()
  const detail = `${reason}: /private/internal/secret; stderr: confidential`
  exec.mockRejectedValue(Object.assign(new Error(detail), { stderr: detail }))
  const result = await installBundledCli({ ...f, packaged: true, confirm: async () => true })
  expect(result).toEqual({ state: 'failed' })
  expect(JSON.stringify(result)).not.toMatch(/private|secret|stderr|confidential/)
})

it('rechecks the destination identity inside the executing script', async () => {
  const f = await fixture()
  await writeFile(f.destination, 'old CLI')
  exec.mockImplementation(async (file, args, options) => {
    const replacement = path.join(root, 'replacement')
    await writeFile(replacement, 'concurrent CLI')
    await rename(replacement, f.destination)
    return run(file, args, options)
  })
  await expect(installLink({ ...f, confirm: async () => true })).rejects.toMatchObject({ code: 1 })
  expect(await readFile(f.destination, 'utf8')).toBe('concurrent CLI')
})
it('rejects target redirection introduced immediately before executing the script', async () => {
  const f = await fixture()
  exec.mockImplementation(async (file, args, options) => {
    const external = path.join(root, 'external')
    await writeFile(external, 'external CLI', { mode: 0o755 })
    await rm(f.target)
    await symlink(external, f.target)
    return run(file, args, options)
  })
  await expect(installLink({ ...f, confirm: async () => true })).rejects.toMatchObject({ code: 1 })
  await expect(readFile(f.destination)).rejects.toMatchObject({ code: 'ENOENT' })
})
