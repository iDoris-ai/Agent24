import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { chmod, mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { tmpdir } from 'node:os'

const exec = vi.fn()
let installedTarget = ''
vi.mock('node:child_process', () => ({
  execFile: (file: string, args: string[], options: unknown, callback: (error: Error | null, stdout: string, stderr: string) => void) => {
    const error: Error | null = exec(file, args, options)
    callback(error, '', '')
    return {}
  },
}))
vi.mock('node:fs/promises', async (load) => {
  const fs = await load<typeof import('node:fs/promises')>()
  return {
    ...fs,
    lstat: (file: string, options: { bigint?: boolean }) => {
      if (file === '/usr/local/bin/agent24') return Promise.reject(Object.assign(new Error('missing'), { code: 'ENOENT' }))
      if (['/usr/local/bin', '/usr/local', '/usr'].includes(file)) return fs.lstat('/', options)
      return fs.lstat(file, options)
    },
    readlink: (file: string) => file === '/usr/local/bin/agent24' ? Promise.resolve(installedTarget) : fs.readlink(file),
  }
})
import { installBundledCli } from './cli-tool'

let root = ''
beforeEach(() => { exec.mockReset() })
afterEach(async () => { if (root) await rm(root, { recursive: true, force: true }) })

// Simulate only OS permission/auth responses. Real bundle resolution still runs;
// neither the shell nor osascript is executed and /usr/local is never modified.
describe.skipIf(process.platform !== 'darwin')('administrator authorization', () => {
  async function install() {
    root = await realpath(await mkdtemp(path.join(tmpdir(), 'a24-auth-')))
    const bundle = path.join(root, 'Agent24.app/Contents')
    const executable = path.join(bundle, 'MacOS/Agent24')
    const resources = path.join(bundle, 'Resources')
    installedTarget = path.join(resources, 'backend/agent24')
    await mkdir(path.dirname(executable), { recursive: true })
    await mkdir(path.dirname(installedTarget), { recursive: true })
    await writeFile(executable, '')
    await writeFile(installedTarget, 'CLI')
    await chmod(installedTarget, 0o755)
    return installBundledCli({ executable, resources, packaged: true, confirm: async () => true })
  }
  it('requests elevation only after a permission error, with a fixed destination', async () => {
    exec.mockImplementation((file: string) => file === '/bin/sh' ? new Error('Permission denied') : null)
    expect(await install()).toEqual({ state: 'installed', backup: null })
    expect(exec.mock.calls.map((call) => call[0])).toEqual(['/bin/sh', '/usr/bin/osascript'])
    expect(exec.mock.calls[1][1][0]).toBe('-e')
    expect(exec.mock.calls[1][1][1]).toContain('/usr/local/bin/agent24')
    expect(exec.mock.calls[1][1][1]).toContain('with administrator privileges')
  })
  it('turns macOS error -128 into a clear cancellation result', async () => {
    exec.mockImplementation((file: string) => new Error(file === '/bin/sh' ? 'Permission denied' : 'User canceled. (-128)'))
    expect(await install()).toEqual({ state: 'cancelled' })
  })
  it('does not request administrator access for other failures', async () => {
    exec.mockReturnValue(new Error('Destination changed'))
    expect(await install()).toEqual({ state: 'failed' })
    expect(exec).toHaveBeenCalledOnce()
  })
})
