import { afterEach, describe, expect, it, vi } from 'vitest'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { chmod, lstat, mkdir, mkdtemp, readFile, readlink, realpath, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { compareVersions, resolveBundledCli, inspectCommand, installLink, shellQuote, appleScript } from './cli-tool'

const run = promisify(execFile)
const roots: string[] = []
async function fixture() {
  const root = await realpath(await mkdtemp(path.join(tmpdir(), 'a24-cli-')))
  roots.push(root)
  const bundle = path.join(root, "Agent24's 测试.app")
  const executable = path.join(bundle, 'Contents/MacOS/Agent24')
  const resources = path.join(bundle, 'Contents/Resources')
  const target = path.join(resources, 'backend/agent24')
  await mkdir(path.dirname(executable), { recursive: true })
  await mkdir(path.dirname(target), { recursive: true })
  await writeFile(executable, '')
  await writeFile(target, '#!/bin/sh\necho "agent24 0.6.0"\n')
  await chmod(target, 0o755)
  const destination = path.join(root, 'bin/agent24')
  return { root, executable, resources, target, destination }
}
afterEach(async () => { await Promise.all(roots.splice(0).map((root) => rm(root, { recursive: true, force: true }))) })

describe('CLI versions', () => {
  it.each([
    ['0.3.0', '0.6.0', -1], ['0.10.0', '0.6.0', 1], ['1.0.0', '0.99.9', 1],
    ['0.6.0+build.1', '0.6.0', 0], ['0.6.0-rc.1', '0.6.0', -1],
    ['0.6.0-rc.2', '0.6.0-rc.10', -1], ['0.6.0-alpha', '0.6.0-beta', -1],
    ['0.6.0', '0.6.0', 0], ['garbage', '0.6.0', null],
    ['0.6.0-rc.01', '0.6.0', null], ['0.6.0-rc..1', '0.6.0', null], ['0.6.0+.', '0.6.0', null],
  ])('%s vs %s', (a, b, expected) => expect(compareVersions(a, b)).toBe(expected))
})

describe('bundled target', () => {
  it('resolves only the fixed executable inside this App', async () => {
    const f = await fixture()
    expect(await resolveBundledCli(f.executable, f.resources)).toBe(f.target)
    await rm(f.target)
    const external = path.join(f.root, 'external')
    await writeFile(external, 'outside')
    await symlink(external, f.target)
    await expect(resolveBundledCli(f.executable, f.resources)).rejects.toThrow()
  })
  it('rejects executables outside an App bundle', async () => {
    const f = await fixture()
    const outside = path.join(f.root, 'Agent24')
    await writeFile(outside, '')
    await expect(resolveBundledCli(outside, f.resources)).rejects.toThrow('Invalid App bundle')
  })
  it('rejects a directory in place of the bundled CLI', async () => {
    const f = await fixture()
    await rm(f.target)
    await mkdir(f.target)
    await expect(resolveBundledCli(f.executable, f.resources)).rejects.toThrow('CLI is not a file')
  })
  it('rejects redirected backend directories and mismatched resources', async () => {
    const f = await fixture()
    await expect(resolveBundledCli(f.executable, f.root)).rejects.toThrow()
    const backend = path.dirname(f.target)
    await rm(backend, { recursive: true })
    const other = path.join(f.root, 'other')
    await mkdir(other)
    await writeFile(path.join(other, 'agent24'), 'outside')
    await symlink(other, backend)
    await expect(resolveBundledCli(f.executable, f.resources)).rejects.toThrow()
  })
})

describe('PATH detection and installation', () => {
  it('detects the first executable on PATH and its real version', async () => {
    const f = await fixture()
    const first = path.join(f.root, 'first')
    await mkdir(first)
    await writeFile(path.join(first, 'agent24'), '#!/bin/sh\necho "agent24 0.3.0"\n', { mode: 0o755 })
    expect(await inspectCommand(`${first}:${path.dirname(f.target)}`)).toEqual({ path: `${first}/agent24`, version: '0.3.0' })
    expect(await inspectCommand(f.root)).toEqual({ path: null, version: null })
  })
  it('backs up the old file, installs a symlink, and updates with the App', async () => {
    const f = await fixture()
    await mkdir(path.dirname(f.destination))
    await writeFile(f.destination, 'old CLI')
    const confirm = vi.fn(async () => true)
    const result = await installLink({ target: f.target, destination: f.destination, confirm })
    expect(result.state).toBe('installed')
    if (result.state !== 'installed' || !result.backup) throw new Error('missing backup')
    expect(result.backup).toMatch(/agent24\.bak-\d{4}-/)
    expect(await readFile(result.backup, 'utf8')).toBe('old CLI')
    expect(await readlink(f.destination)).toBe(f.target)
    expect(confirm).toHaveBeenCalledOnce()
    await writeFile(f.target, '#!/bin/sh\necho "agent24 0.7.0"\n')
    expect((await run(f.destination, ['--version'])).stdout.trim()).toBe('agent24 0.7.0')
    expect((await installLink({ target: f.target, destination: f.destination, confirm })).state).toBe('installed')
    expect(confirm).toHaveBeenCalledOnce()
  })
  it('leaves the old file alone when replacement is cancelled', async () => {
    const f = await fixture()
    await mkdir(path.dirname(f.destination))
    await writeFile(f.destination, 'old CLI')
    expect(await installLink({ target: f.target, destination: f.destination, confirm: async () => false })).toEqual({ state: 'cancelled' })
    expect(await readFile(f.destination, 'utf8')).toBe('old CLI')
  })
  it('backs up a live foreign symlink at a system destination without changing its target', async () => {
    const f = await fixture()
    // Exercise the system policy in a temporary bin, without writing /usr/local/bin.
    const destination = path.join(f.root, 'usr/local/bin/agent24')
    const original = path.join(f.root, 'developer-cli')
    await mkdir(path.dirname(destination), { recursive: true })
    await writeFile(original, '#!/bin/sh\necho "agent24 0.4.0"\n', { mode: 0o755 })
    await symlink(original, destination)
    const before = await lstat(destination)
    const confirm = vi.fn(async () => true)
    const result = await installLink({ target: f.target, destination, confirm })
    expect(result.state).toBe('installed')
    if (result.state !== 'installed') throw new Error('installation failed')
    expect(result.backup).not.toBeNull()
    if (!result.backup) throw new Error('missing backup')
    expect(result.backup).toMatch(/agent24\.bak-/)
    expect((await lstat(result.backup)).isSymbolicLink()).toBe(true)
    expect((await lstat(result.backup)).ino).toBe(before.ino)
    expect(await readlink(result.backup)).toBe(original)
    expect(await realpath(result.backup)).toBe(original)
    expect((await run(result.backup, ['--version'])).stdout.trim()).toBe('agent24 0.4.0')
    expect(await readlink(destination)).toBe(f.target)
    expect(confirm).toHaveBeenCalledOnce()
  })
  it('backs up a dangling symlink at a system destination', async () => {
    const f = await fixture()
    await mkdir(path.dirname(f.destination))
    await symlink('/missing/old-agent24', f.destination)
    const result = await installLink({ target: f.target, destination: f.destination, confirm: async () => true })
    expect(result.state).toBe('installed')
    if (result.state !== 'installed' || !result.backup) throw new Error('missing backup')
    expect(await readlink(result.backup)).toBe('/missing/old-agent24')
    expect(await readlink(f.destination)).toBe(f.target)
  })
  it('rejects destination parent symlinks and directories', async () => {
    const f = await fixture()
    await symlink(path.dirname(f.target), path.dirname(f.destination))
    await expect(installLink({ target: f.target, destination: f.destination, confirm: async () => true })).rejects.toThrow('Symlinked parent directory')
    await rm(path.dirname(f.destination))
    await mkdir(f.destination, { recursive: true })
    await expect(installLink({ target: f.target, destination: f.destination, confirm: async () => true })).rejects.toThrow('Destination is not a file or symlink')
  })
  it('rejects a replacement changed during confirmation', async () => {
    const f = await fixture()
    await mkdir(path.dirname(f.destination))
    await writeFile(f.destination, 'old CLI')
    await expect(installLink({ target: f.target, destination: f.destination, confirm: async () => {
      await rm(f.destination)
      await writeFile(f.destination, 'different CLI')
      return true
    } })).rejects.toThrow('Installation paths changed')
    expect(await readFile(f.destination, 'utf8')).toBe('different CLI')
  })
  it('rejects a symlink target before asking for replacement', async () => {
    const f = await fixture()
    const alias = path.join(f.root, 'alias')
    await symlink(f.target, alias)
    const confirm = vi.fn(async () => true)
    await expect(installLink({ target: alias, destination: f.destination, confirm })).rejects.toThrow('Invalid target')
    expect(confirm).not.toHaveBeenCalled()
  })
  it('quotes shell and AppleScript metacharacters as data', async () => {
    const value = "a'\"\\$() `touch nope` ไทย"
    expect((await run('/bin/sh', ['-c', `printf %s ${shellQuote(value)}`])).stdout).toBe(value)
    expect(appleScript('echo "x" \\')).toBe('do shell script "echo \\"x\\" \\\\" with administrator privileges')
    expect(() => shellQuote('bad\npath')).toThrow()
  })
})
