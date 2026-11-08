import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { knowledgeAppInstanceId } from './knowledge-app-instance'

const dirs: string[] = []
function tempDir(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kb-app-instance-'))
  dirs.push(dir)
  return dir
}
afterEach(() => {
  for (const dir of dirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true })
})

describe('knowledgeAppInstanceId (design §2.1)', () => {
  it('makes a random 32-byte id once and keeps it, in a 0600 file', () => {
    const dir = tempDir()
    const first = knowledgeAppInstanceId(dir)
    expect(first).toMatch(/^[A-Za-z0-9_-]{43}$/)
    expect(knowledgeAppInstanceId(dir)).toBe(first)
    const file = path.join(dir, 'knowledge-app-instance.json')
    expect(fs.statSync(file).mode & 0o777).toBe(0o600)
    expect(fs.readdirSync(dir)).toEqual(['knowledge-app-instance.json'])
    expect(knowledgeAppInstanceId(tempDir())).not.toBe(first)
  })

  it('uses the random source it is given', () => {
    const dir = tempDir()
    expect(knowledgeAppInstanceId(dir, (n) => new Uint8Array(n).fill(1))).toBe(Buffer.alloc(32, 1).toString('base64url'))
  })

  it('replaces a file it cannot use', () => {
    const dir = tempDir()
    const file = path.join(dir, 'knowledge-app-instance.json')
    for (const content of ['not json', JSON.stringify({ id: 'short' }), JSON.stringify(null), JSON.stringify({ id: 'a'.repeat(42) + '=' })]) {
      fs.writeFileSync(file, content)
      const id = knowledgeAppInstanceId(dir)
      expect(id).toMatch(/^[A-Za-z0-9_-]{43}$/)
      expect(knowledgeAppInstanceId(dir)).toBe(id)
    }
  })
})
