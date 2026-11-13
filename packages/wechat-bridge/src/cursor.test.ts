import { spawnSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterEach, describe, expect, it } from 'vitest'
import { ilinkAccountKey, loadBridgeCursor, saveBridgeCursor } from './config.js'

const dirs: string[] = []
afterEach(() => {
  delete process.env.AGENT24_DATA_DIR
  for (const dir of dirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true })
})

function dataDir(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'bridge-cursor-'))
  const dev = path.join(dir, 'dev')
  fs.mkdirSync(dev, { mode: 0o700 })
  fs.chmodSync(dev, 0o700)
  process.env.AGENT24_DATA_DIR = dir
  dirs.push(dir)
  return dir
}

describe('bridge cursor account', () => {
  it('ignores a cursor saved for another iLink account', () => {
    dataDir()
    const errors: string[] = []
    const previous = console.error
    console.error = (...args: unknown[]) => {
      errors.push(args.map(String).join(' '))
    }
    try {
      saveBridgeCursor('11', 'bot-a')
      expect(loadBridgeCursor('bot-a')).toBe('11')
      expect(loadBridgeCursor('bot-b')).toBe('')
      expect(errors.join('\n')).toContain('忽略旧游标')
      expect(ilinkAccountKey('same')).toBe(ilinkAccountKey('same'))
      expect(ilinkAccountKey('same')).not.toBe(ilinkAccountKey('other'))
    } finally {
      console.error = previous
    }
  })

  it('refuses a fifo token file without blocking', () => {
    const dir = dataDir()
    const dev = path.join(dir, 'dev')
    const fifo = path.join(dev, 'wechat-bridge.token')
    expect(spawnSync('mkfifo', [fifo]).status).toBe(0)
    const config = fileURLToPath(new URL('./config.ts', import.meta.url))
    const probe = path.join(dir, 'probe.ts')
    fs.writeFileSync(
      probe,
      `import { loadBridgeCredential } from ${JSON.stringify(config)}\n` +
        `try { loadBridgeCredential(1); console.log('OPENED') } catch (err) { console.log('ERR ' + (err instanceof Error ? err.message : err)) }\n`,
    )
    const result = spawnSync(process.execPath, ['--import', 'tsx', probe], {
      cwd: fileURLToPath(new URL('..', import.meta.url)),
      env: { ...process.env, AGENT24_DATA_DIR: dir },
      encoding: 'utf8',
      timeout: 2000,
    })
    const code = result.error && 'code' in result.error ? result.error.code : undefined
    expect(code).not.toBe('ETIMEDOUT')
    expect(result.status).toBe(0)
    expect(result.stdout).toContain('ERR')
    expect(result.stdout).not.toContain('OPENED')
  })
})
