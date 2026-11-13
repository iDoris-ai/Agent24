import { spawnSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

const entry = fileURLToPath(new URL('./main.ts', import.meta.url))
function start(marker: string | undefined) {
  const data = fs.mkdtempSync(path.join(os.tmpdir(), 'bridge-main-'))
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    AGENT24_DATA_DIR: data,
    A24_BASE_URL: 'invalid-url',
  }
  delete env.A24_TOKEN
  delete env.A24_DEV_TOOL
  if (marker !== undefined) env.A24_DEV_TOOL = marker
  return spawnSync(process.execPath, ['--import', 'tsx', entry], {
    env, encoding: 'utf8', timeout: 10_000,
  })
}

describe('development-only startup (ADR-044 T3)', () => {
  it.each([undefined, '', '0', 'true'])('rejects marker %s before daemon discovery', (marker) => {
    const result = start(marker)
    expect(result.status).toBe(1)
    expect(result.stderr).toContain('A24_DEV_TOOL=1')
    expect(result.stderr).not.toContain('A24_BASE_URL is not a valid URL')
  })

  it('explicit development opt-in stops on the missing token file', () => {
    const result = start('1')
    expect(result.status).toBe(1)
    expect(result.stderr).toContain('agent24 dev bridge-token')
    expect(result.stderr).not.toContain('A24_BASE_URL is not a valid URL')
    expect(result.stderr).not.toContain('A24_DEV_TOOL=1')
  })
})
