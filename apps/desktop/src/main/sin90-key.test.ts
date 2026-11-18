import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { expandDataDir, humanKey } from './sin90-key'

const HUMAN = 'h'.repeat(48)
const AUTO = 'a'.repeat(48)

describe('expandDataDir', () => {
  it('expands a home path and rejects traversal', () => {
    expect(expandDataDir('~/.agent24/os/sin90/', '/Users/me')).toBe('/Users/me/.agent24/os/sin90')
    expect(expandDataDir('/tmp/sin90', '/Users/me')).toBe('/tmp/sin90')
    expect(expandDataDir('~/.agent24/../etc', '/Users/me')).toBeNull()
    expect(expandDataDir('relative/sin90', '/Users/me')).toBeNull()
  })
})

describe('humanKey', () => {
  const dirs: string[] = []
  afterEach(() => { for (const dir of dirs) fs.rmSync(dir, { recursive: true, force: true }) })

  it('reads human from a 0600 file owned by this user and refuses a looser mode', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'sin90-key-'))
    dirs.push(root)
    const data = path.join(root, 'os')
    fs.mkdirSync(data)
    const file = path.join(data, 'actor-keys.json')
    fs.writeFileSync(file, JSON.stringify({ human: HUMAN, automation: AUTO }), { mode: 0o600 })
    const uid = process.getuid?.() ?? -1
    const manifest = `name: sin90\ndata_dir: ${data}\n`
    const files = {
      manifestText: manifest,
      home: '/Users/me',
      uid,
      stat: (p: string) => {
        const info = fs.statSync(p)
        return { mode: info.mode, uid: info.uid }
      },
      readFile: (p: string) => fs.readFileSync(p, 'utf8'),
    }
    expect(humanKey(files)).toEqual({ ok: true, human: HUMAN })
    fs.chmodSync(file, 0o644)
    const loose = humanKey(files)
    expect(loose.ok).toBe(false)
    expect(JSON.stringify(loose)).not.toContain(HUMAN)
  })

  it('refuses a file owned by someone else', () => {
    const result = humanKey({
      manifestText: 'data_dir: /tmp/sin90\n',
      home: '/Users/me',
      uid: 1,
      stat: () => ({ mode: 0o600, uid: 2 }),
      readFile: () => JSON.stringify({ human: HUMAN, automation: AUTO }),
    })
    expect(result).toEqual({ ok: false })
  })
})
