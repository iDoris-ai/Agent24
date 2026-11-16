// Model source setting: a local OpenAI-compatible endpoint (LM Studio's
// http://127.0.0.1:1234, …) and a model name, kept in
// `<data_dir>/model-source.json`. The daemon reads it when it builds its model
// router (agent24-models `read_model_source`, which checks it again and puts
// it first in the local chain), so a change takes effect on a daemon restart.

import fs from 'node:fs'
import path from 'node:path'
import { parseLoopbackUrl } from './omlx-validate'
import type { ModelSource } from '../shared/ipc-types'

export const MODEL_SOURCE_FILE = 'model-source.json'
const MAX_MODEL = 256

export function modelSourcePath(dataDir: string): string {
  return path.join(dataDir, MODEL_SOURCE_FILE)
}

/** The bare origin the daemon accepts (`http://127.0.0.1:1234`): a loopback
 * http(s) URL with no credentials, query or fragment; a trailing `/v1` or `/`
 * is dropped (the daemon adds `/v1/...` itself). Null for anything else. */
export function normalizeModelSourceUrl(input: unknown): string | null {
  if (typeof input !== 'string') return null
  const url = parseLoopbackUrl(input.trim())
  if (!url || url.search !== '' || url.hash !== '') return null
  const rest = url.pathname.replace(/\/+$/, '').replace(/\/v1$/, '')
  return rest === '' ? url.origin : null
}

export function normalizeModelName(input: unknown): string | null {
  if (typeof input !== 'string') return null
  const model = input.trim()
  // eslint-disable-next-line no-control-regex
  return model !== '' && model.length <= MAX_MODEL && !/[\u0000-\u001f\u007f]/.test(model) ? model : null
}

export function readModelSource(file: string): ModelSource | null {
  try {
    const value = JSON.parse(fs.readFileSync(file, 'utf8')) as { url?: unknown; model?: unknown }
    const url = normalizeModelSourceUrl(value.url)
    const model = normalizeModelName(value.model)
    return url && model ? { url, model } : null
  } catch {
    return null
  }
}

/** Written next to the target and renamed over it, owner-only (0600). */
export function writeModelSource(file: string, source: ModelSource): void {
  fs.mkdirSync(path.dirname(file), { recursive: true })
  const temp = path.join(path.dirname(file), `.model-source.${process.pid}.tmp`)
  fs.rmSync(temp, { force: true })
  try {
    const fd = fs.openSync(temp, 'wx', 0o600)
    try {
      fs.writeFileSync(fd, JSON.stringify({ url: source.url, model: source.model }))
      fs.fchmodSync(fd, 0o600)
      fs.fsyncSync(fd)
    } finally {
      fs.closeSync(fd)
    }
    fs.renameSync(temp, file)
  } catch (err) {
    fs.rmSync(temp, { force: true })
    throw err
  }
}

export function clearModelSource(file: string): void {
  fs.rmSync(file, { force: true })
}
