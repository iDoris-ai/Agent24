import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { APP_DAEMON_LOG, appDaemonLog } from './backend-manager'

const dirs: string[] = []
afterEach(() => { for (const d of dirs) fs.rmSync(d, { recursive: true, force: true }); dirs.length = 0 })

describe('appDaemonLog (Loop 4: the app-run daemon logs to ~/.agent24/logs)', () => {
  it('appends to logs/agent24d.app.log, owner-only, and moves a large one aside', async () => {
    const data = fs.mkdtempSync(path.join(os.tmpdir(), 'a24log-'))
    dirs.push(data)
    const file = path.join(data, 'logs', APP_DAEMON_LOG)
    for (const line of ['first', 'second']) {
      const s = appDaemonLog(data)!
      s.write(`${line}\n`)
      await new Promise<void>((r) => s.end(r))
    }
    expect(fs.readFileSync(file, 'utf8')).toBe('first\nsecond\n')
    expect(fs.statSync(file).mode & 0o777).toBe(0o600)
    fs.writeFileSync(file, Buffer.alloc(21 * 1024 * 1024))
    const s = appDaemonLog(data)!
    s.write('fresh\n')
    await new Promise<void>((r) => s.end(r))
    expect(fs.readFileSync(file, 'utf8')).toBe('fresh\n')
    expect(fs.existsSync(`${file}.1`)).toBe(true)
    expect(appDaemonLog(null)).toBeNull()
  })
})
