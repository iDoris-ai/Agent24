import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { KnowledgeLoginStore, encryptionUsable, type KnowledgeLogin, type SecretBox } from './knowledge-logins'

/** Reversible stand-in for safeStorage; the ciphertext never contains the plain text. */
function fakeBox(overrides: Partial<SecretBox> = {}): SecretBox {
  return {
    isEncryptionAvailable: () => true,
    encryptString: (plain) => Buffer.from(`enc:${Buffer.from(plain).toString('base64')}`),
    decryptString: (cipher) => {
      const text = cipher.toString()
      if (!text.startsWith('enc:')) throw new Error('bad ciphertext')
      return Buffer.from(text.slice(4), 'base64').toString()
    },
    ...overrides,
  }
}

const login = (material: string, extra: Partial<KnowledgeLogin> = {}): KnowledgeLogin => ({
  profileId: 'p1',
  material,
  unusable: false,
  temporary: false,
  ...extra,
})

describe('KnowledgeLoginStore (KB-HOST-01 PR-B2a)', () => {
  let dir = ''
  const file = () => path.join(dir, 'knowledge-logins.bin')

  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kb-logins-'))
  })
  afterEach(() => {
    fs.rmSync(dir, { recursive: true, force: true })
  })

  it('round-trips every login through one encrypted file, never in the clear', async () => {
    const store = new KnowledgeLoginStore(dir, fakeBox(), 'darwin')
    expect(store.load()).toEqual({ ok: true })
    await store.put('b1', login('REFRESH-1'))
    await store.put('b2', login('REFRESH-2', { profileId: 'p2', temporary: true }))
    expect(fs.readFileSync(file()).toString()).not.toContain('REFRESH')
    expect(fs.statSync(file()).mode & 0o777).toBe(0o600)

    const again = new KnowledgeLoginStore(dir, fakeBox(), 'darwin')
    expect(again.load()).toEqual({ ok: true })
    expect([...again.list()]).toEqual([
      ['b1', login('REFRESH-1')],
      ['b2', login('REFRESH-2', { profileId: 'p2', temporary: true })],
    ])
  })

  it.each([
    ['not decryptable', 'garbage'],
    ['not JSON', `enc:${Buffer.from('{').toString('base64')}`],
    ['wrong version', `enc:${Buffer.from(JSON.stringify({ v: 2, logins: {} })).toString('base64')}`],
    ['logins not a map', `enc:${Buffer.from(JSON.stringify({ v: 1, logins: [] })).toString('base64')}`],
    ['an entry of the wrong shape', `enc:${Buffer.from(JSON.stringify({ v: 1, logins: { b1: { material: 'x' } } })).toString('base64')}`],
    ['an entry with an extra field', `enc:${Buffer.from(JSON.stringify({ v: 1, logins: { b1: { ...login('x'), extra: 1 } } })).toString('base64')}`],
  ])('treats every login as gone when the file is %s, and a later save replaces it', async (_, content) => {
    fs.writeFileSync(file(), content)
    const store = new KnowledgeLoginStore(dir, fakeBox(), 'darwin')
    expect(store.load()).toEqual({ ok: false, reason: 'unreadable' })
    expect(store.list().size).toBe(0)

    await store.put('b9', login('NEW'))
    const again = new KnowledgeLoginStore(dir, fakeBox(), 'darwin')
    expect(again.load()).toEqual({ ok: true })
    expect([...again.list().keys()]).toEqual(['b9'])
  })

  it('refuses to load or save without usable secure storage', async () => {
    const store = new KnowledgeLoginStore(dir, fakeBox({ isEncryptionAvailable: () => false }), 'darwin')
    expect(store.load()).toEqual({ ok: false, reason: 'encryption_unavailable' })
    await expect(store.put('b1', login('REFRESH'))).rejects.toThrow('secure storage is unavailable')
    expect(fs.existsSync(file())).toBe(false)
  })

  it('counts Linux basic_text (or an unknown backend) as no encryption', () => {
    expect(encryptionUsable(fakeBox({ getSelectedStorageBackend: () => 'basic_text' }), 'linux')).toBe(false)
    expect(encryptionUsable(fakeBox(), 'linux')).toBe(false)
    expect(encryptionUsable(fakeBox({ getSelectedStorageBackend: () => 'gnome_libsecret' }), 'linux')).toBe(true)
    expect(encryptionUsable(fakeBox(), 'darwin')).toBe(true)
  })

  it('a new login counts only once it is on disk; unusable and removed hold even when the write fails', async () => {
    let failWrites = false
    const box = fakeBox({
      encryptString: (plain) => {
        if (failWrites) throw new Error('keychain locked')
        return Buffer.from(`enc:${Buffer.from(plain).toString('base64')}`)
      },
    })
    const store = new KnowledgeLoginStore(dir, box, 'darwin')
    store.load()
    await store.put('b1', login('R1'))
    await store.put('b2', login('R2'))

    failWrites = true
    await expect(store.put('b3', login('R3'))).rejects.toThrow('keychain locked')
    expect(store.list().has('b3')).toBe(false)
    await expect(store.markUnusable('b1')).rejects.toThrow('keychain locked')
    expect(store.list().get('b1')?.unusable).toBe(true)
    await expect(store.remove('b2')).rejects.toThrow('keychain locked')
    expect(store.list().has('b2')).toBe(false)

    // The next successful write carries both tightenings to disk.
    failWrites = false
    await store.put('b4', login('R4'))
    const again = new KnowledgeLoginStore(dir, box, 'darwin')
    again.load()
    expect([...again.list()]).toEqual([
      ['b1', login('R1', { unusable: true })],
      ['b4', login('R4')],
    ])
  })

  it('writes one at a time in call order, so no update is lost', async () => {
    const store = new KnowledgeLoginStore(dir, fakeBox(), 'darwin')
    store.load()
    await Promise.all(Array.from({ length: 20 }, (_, i) => store.put(`b${i}`, login(`R${i}`))))
    const again = new KnowledgeLoginStore(dir, fakeBox(), 'darwin')
    again.load()
    expect(again.list().size).toBe(20)
  })

  it('clears a temporary file left by a crash before reading', () => {
    fs.writeFileSync(`${file()}.tmp`, 'partial')
    new KnowledgeLoginStore(dir, fakeBox(), 'darwin').load()
    expect(fs.existsSync(`${file()}.tmp`)).toBe(false)
  })

  it('never logs the material', async () => {
    const spies = (['log', 'warn', 'error', 'info', 'debug'] as const).map((m) => vi.spyOn(console, m).mockImplementation(() => {}))
    fs.writeFileSync(file(), 'garbage')
    const store = new KnowledgeLoginStore(dir, fakeBox(), 'darwin')
    store.load()
    await store.put('b1', login('REFRESH-SECRET'))
    for (const spy of spies) expect(JSON.stringify(spy.mock.calls)).not.toContain('REFRESH-SECRET')
    spies.forEach((s) => s.mockRestore())
  })
})
