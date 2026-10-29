import fs from 'node:fs'
import path from 'node:path'
import { describe, expect, it } from 'vitest'

describe('IL-13 daemon owns iDoris Admin credentials', () => {
  it('removes the inherited token FD reader, launch handoff and dedicated renderer IPC', () => {
    const sources = ['main.ts', 'preload.ts', 'ipc/index.ts'].map(file => fs.readFileSync(path.join(__dirname, file), 'utf8')).join('\n')
    expect(sources).not.toMatch(/TOKEN_ARG|--idoris-admin-token-fd|createIdorisStatusProvider|idorisStatusProviderPromise|IpcChannels\.IdorisStatus/)
    expect(fs.existsSync(path.join(__dirname, 'idoris-admin.ts'))).toBe(false)
    expect(fs.readFileSync(path.join(__dirname, '../renderer/pages/Models.tsx'), 'utf8')).toContain('<IdorisPanel')
  })
})
