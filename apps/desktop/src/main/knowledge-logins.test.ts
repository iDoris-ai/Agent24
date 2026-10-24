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

  it('clears temporary files left by a crash before reading', () => {
    fs.writeFileSync(`${file()}.0123abcd.tmp`, 'partial')
    fs.writeFileSync(path.join(dir, 'unrelated.tmp'), 'keep')
    new KnowledgeLoginStore(dir, fakeBox(), 'darwin').load()
    expect(fs.readdirSync(dir)).toEqual(['unrelated.tmp'])
  })

  it('creates a missing directory private to the user', async () => {
    const nested = path.join(dir, 'a', 'b')
    const store = new KnowledgeLoginStore(nested, fakeBox(), 'darwin')
    store.load()
    await store.put('b1', login('R1'))
    expect(fs.statSync(nested).mode & 0o777).toBe(0o700)
    expect(fs.readdirSync(nested)).toEqual(['knowledge-logins.bin'])
  })

  it('a tightening made while a put is being written survives that put, in memory and on disk', async () => {
    let store: KnowledgeLoginStore | null = null
    let tightenOnce: (() => void) | null = null
    const box = fakeBox({
      encryptString: (plain) => {
        // Runs inside the put's write, after it took its snapshot.
        const hook = tightenOnce
        tightenOnce = null
        hook?.()
        return Buffer.from(`enc:${Buffer.from(plain).toString('base64')}`)
      },
    })
    store = new KnowledgeLoginStore(dir, box, 'darwin')
    store.load()
    await store.put('b1', login('R1'))
    await store.put('b3', login('R3'))

    let tightening: Promise<void>[] = []
    tightenOnce = () => {
      tightening = [store!.markUnusable('b1'), store!.remove('b3')]
    }
    await store.put('b2', login('R2'))
    await Promise.all(tightening)

    expect(store.list().get('b1')?.unusable).toBe(true)
    expect(store.list().has('b3')).toBe(false)
    expect(store.list().get('b2')).toEqual(login('R2'))
    const again = new KnowledgeLoginStore(dir, box, 'darwin')
    again.load()
    expect([...again.list()]).toEqual([
      ['b1', login('R1', { unusable: true })],
      ['b2', login('R2')],
    ])
  })

  it.each(['markUnusable', 'remove'] as const)('a %s of the binding while its put is written wins', async (tighten) => {
    let hook: (() => void) | null = null
    const box = fakeBox({
      encryptString: (plain) => {
        const run = hook
        hook = null
        run?.()
        return Buffer.from(`enc:${Buffer.from(plain).toString('base64')}`)
      },
    })
    const store = new KnowledgeLoginStore(dir, box, 'darwin')
    store.load()
    await store.put('b1', login('OLD'))
    let tightening: Promise<void> = Promise.resolve()
    hook = () => {
      tightening = store[tighten]('b1')
    }
    await expect(store.put('b1', login('NEW'))).rejects.toThrow('superseded')
    await tightening
    const expected = tighten === 'remove' ? undefined : login('OLD', { unusable: true })
    expect(store.list().get('b1')).toEqual(expected)
    const again = new KnowledgeLoginStore(dir, box, 'darwin')
    again.load()
    expect(again.list().get('b1')).toEqual(expected)
  })

  it('a remove queued after a put supersedes it', async () => {
    const store = new KnowledgeLoginStore(dir, fakeBox(), 'darwin')
    store.load()
    await store.put('b1', login('OLD'))
    const replacing = store.put('b1', login('NEW'))
    const removing = store.remove('b1')
    await expect(replacing).rejects.toThrow('superseded')
    await removing
    expect(store.list().has('b1')).toBe(false)
  })

  it('a put is superseded by a later tightening of the same binding, and never reaches the disk', async () => {
    const encrypted: string[] = []
    const box = fakeBox()
    const recording = fakeBox({ encryptString: (plain) => (encrypted.push(plain), box.encryptString(plain)) })
    const store = new KnowledgeLoginStore(dir, recording, 'darwin')
    store.load()
    await store.put('b1', login('OLD'))
    const replacing = store.put('b1', login('NEW'))
    const marking = store.markUnusable('b1')
    await expect(replacing).rejects.toThrow('superseded')
    await marking
    expect(encrypted.some((plain) => plain.includes('NEW'))).toBe(false)
    expect(store.list().get('b1')).toEqual(login('OLD', { unusable: true }))
    const again = new KnowledgeLoginStore(dir, box, 'darwin')
    again.load()
    expect(again.list().get('b1')).toEqual(login('OLD', { unusable: true }))

    // A put called after the tightening is a fresh login and counts.
    await store.put('b1', login('RELOGIN'))
    expect(store.list().get('b1')).toEqual(login('RELOGIN'))
  })

  it('keeps what the put was called with, even when the caller changes it afterwards', async () => {
    const store = new KnowledgeLoginStore(dir, fakeBox(), 'darwin')
    store.load()
    const value = login('CALLED')
    const putting = store.put('b1', value)
    value.material = 'CHANGED'
    await putting
    expect(store.list().get('b1')).toEqual(login('CALLED'))
    const again = new KnowledgeLoginStore(dir, fakeBox(), 'darwin')
    again.load()
    expect(again.list().get('b1')).toEqual(login('CALLED'))
  })

  it('stores exactly the four fields, so a login carrying more does not make the file unreadable', async () => {
    const store = new KnowledgeLoginStore(dir, fakeBox(), 'darwin')
    store.load()
    await store.put('a', login('A'))
    const carryingMore = { ...login('B'), expiresAt: 1 }
    await store.put('b', carryingMore)
    expect(store.list().get('b')).toEqual(login('B'))
    const again = new KnowledgeLoginStore(dir, fakeBox(), 'darwin')
    expect(again.load()).toEqual({ ok: true })
    expect([...again.list().keys()].sort()).toEqual(['a', 'b'])
    expect(again.list().get('b')).toEqual(login('B'))
  })

  it('a secure storage that throws counts as unavailable instead of failing the load', () => {
    const throwing = fakeBox({
      getSelectedStorageBackend: () => {
        throw new Error('backend unknown')
      },
    })
    expect(new KnowledgeLoginStore(dir, throwing, 'linux').load()).toEqual({ ok: false, reason: 'encryption_unavailable' })
    const unavailable = fakeBox({
      isEncryptionAvailable: () => {
        throw new Error('not ready')
      },
    })
    expect(new KnowledgeLoginStore(dir, unavailable, 'darwin').load()).toEqual({ ok: false, reason: 'encryption_unavailable' })
  })

  it('a directory it cannot list does not fail the load', () => {
    const notADir = path.join(dir, 'not-a-dir')
    fs.writeFileSync(notADir, '')
    expect(() => new KnowledgeLoginStore(notADir, fakeBox(), 'darwin').load()).not.toThrow()
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
