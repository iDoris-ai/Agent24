import { spawnSync } from 'node:child_process'
import crypto from 'node:crypto'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { OpenDesignComponentInstaller, type OpenDesignComponentManifest } from './open-design-component'

// SD-01 R2 independent review follow-up. See docs/agent/ for the R1 fix this
// hardens further; this file never touches the real network, Keychain, mic,
// or the user's actual $HOME — every fixture lives under its own mkdtemp.
// The idle-push test waits out real SPEED_IDLE_MS (3s) gaps, so give it
// headroom past the 5s default.
vi.setConfig({ testTimeout: 15_000 })

const tempDirs: string[] = []
afterEach(() => { for (const dir of tempDirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true }) })
function tmpDir(prefix: string): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix))
  tempDirs.push(dir)
  return dir
}

// Runs the EXACT, unmodified `openRegularFileNoFollow` body straight out of
// the real source file (regex-extracted, TS type annotations stripped — the
// only transformation) inside a short-lived, hard-timed-out child process.
// A FIFO opened for read with the pre-R2 flags blocks the open() syscall
// forever — running that in-process would hang the whole Vitest worker, so
// this out-of-process + timeout harness is what lets the test itself fail
// fast (RED) instead of wedging the suite when the guard regresses.
function runHelperInSubprocess(target: string): { timedOut: boolean; stdout: string } {
  const src = fs.readFileSync(path.join(__dirname, 'open-design-component.ts'), 'utf8')
  const match = src.match(/function openRegularFileNoFollow\([^)]*\)[^{]*\{[\s\S]*?\n\}/)
  if (!match) throw new Error('openRegularFileNoFollow not found in source — review harness is stale')
  const fnSrc = match[0]
    .replace(/function openRegularFileNoFollow\(filePath: string, flags: number, mode\?: number\): number \| null/, 'function openRegularFileNoFollow(filePath, flags, mode)')
    .replace(/let fd: number/, 'let fd')
  const script = `const fs=require('node:fs');${fnSrc}\nprocess.stdout.write(JSON.stringify(openRegularFileNoFollow(process.argv[1], fs.constants.O_RDONLY)))`
  const result = spawnSync(process.execPath, ['-e', script, target], { timeout: 2_000, encoding: 'utf8' })
  return { timedOut: result.signal != null || result.error?.message.includes('ETIMEDOUT') === true, stdout: result.stdout ?? '' }
}

describe('R2: openRegularFileNoFollow vs a planted FIFO', () => {
  it('regular file still opens, symlink still null, FIFO never blocks (real RED→GREEN via bounded subprocess)', () => {
    const dir = tmpDir('a24-r2-fifo-')
    fs.writeFileSync(path.join(dir, 'regular.txt'), 'hello')
    fs.symlinkSync('regular.txt', path.join(dir, 'link.txt'))
    fs.mkfifoSync?.(path.join(dir, 'fifo')) // not on fs typings; fall back below
    if (!fs.existsSync(path.join(dir, 'fifo'))) {
      spawnSync('mkfifo', [path.join(dir, 'fifo')])
    }
    const regular = runHelperInSubprocess(path.join(dir, 'regular.txt'))
    expect(regular.timedOut).toBe(false)
    expect(JSON.parse(regular.stdout)).not.toBeNull()

    const symlink = runHelperInSubprocess(path.join(dir, 'link.txt'))
    expect(symlink.timedOut).toBe(false)
    expect(JSON.parse(symlink.stdout)).toBeNull()

    const fifo = runHelperInSubprocess(path.join(dir, 'fifo'))
    expect(fifo.timedOut).toBe(false) // GREEN: pre-R2 this timed out (blocked) instead of returning null
    expect(JSON.parse(fifo.stdout)).toBeNull()
  })
})

function manifestAt(url: string, size: number, sha256: string): OpenDesignComponentManifest {
  return { version: '0.22.2', odSha: 'f84ff89656143b5fa3fe8f0891b7f2bf38769d9f', platform: process.platform, arch: process.arch, url, sha256, size }
}

describe('R2: idle-push actually reaches onProgress subscribers (not just a later status() poll)', () => {
  it('a stalled download pushes a fresh "waiting for network" update with NO new chunk, then pause leaves no orphan timer', async () => {
    const root = tmpDir('a24-r2-idle-')
    const bytes = Buffer.from('x'.repeat(64))
    const sha256 = crypto.createHash('sha256').update(bytes).digest('hex')
    const manifest = manifestAt('https://example.invalid/asset.tar.gz', bytes.length, sha256)
    let stream!: ReadableStreamDefaultController<Uint8Array>
    const installer = new OpenDesignComponentInstaller(
      { manifest, componentsRoot: root, allowedHosts: ['example.invalid'] },
      async (_input, init) => new Response(new ReadableStream({
        start(c) { stream = c; init?.signal?.addEventListener('abort', () => c.error(init.signal?.reason)) },
      })),
    )
    const states: Array<{ bytesPerSecond?: number; etaSeconds?: number | null }> = []
    const unsubscribe = installer.onProgress((s) => { if (s.state === 'downloading') states.push({ bytesPerSecond: s.bytesPerSecond, etaSeconds: s.etaSeconds }) })
    const pending = installer.install()
    await vi.waitFor(() => expect(stream).toBeDefined())
    stream.enqueue(bytes.subarray(0, 4))
    await new Promise((resolve) => setTimeout(resolve, 150)) // real elapsed time, so the NEXT sample reads a genuine non-zero rate
    stream.enqueue(bytes.subarray(4, 10)) // still far under manifest.size — download stays open, no further chunk ever arrives
    await vi.waitFor(() => expect(states.some((s) => (s.bytesPerSecond ?? 0) > 0)).toBe(true))
    const countAfterRealSample = states.length
    // Nothing sends another chunk from here — only the R2 idle-push timer can grow `states` again.
    const pushedFreshIdle = await new Promise<boolean>((resolve) => {
      const start = Date.now()
      const check = () => {
        if (states.slice(countAfterRealSample).some((s) => s.bytesPerSecond === 0 && s.etaSeconds === null)) return resolve(true)
        if (Date.now() - start > 5_000) return resolve(false)
        setTimeout(check, 100)
      }
      check()
    })
    expect(pushedFreshIdle).toBe(true) // GREEN: pre-R2, `states` never grows past countAfterRealSample without a new chunk
    unsubscribe()
    installer.pause()
    const result = await pending
    expect(result.state).toBe('paused') // confirms the abort actually reached the stream — no leaked job
  })
})
