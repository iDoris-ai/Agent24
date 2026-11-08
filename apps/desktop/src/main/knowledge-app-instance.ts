// KB-C2-04a (#1623): this install's app_instance_id, the first input of the
// UI session's binding_tag (UIB §1, design §2.1). A random value written to
// userData on first use (0600), never derived from a profile, a path or the
// principal (U4.1). A missing or unreadable file gets a new one: that only
// gives new binding tags, which the server records and does not judge.

import { randomBytes } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'

const FILE = 'knowledge-app-instance.json'
const ID = /^[A-Za-z0-9_-]{43}$/

export function knowledgeAppInstanceId(dir: string, random: (n: number) => Uint8Array = randomBytes): string {
  const file = path.join(dir, FILE)
  try {
    const id = (JSON.parse(fs.readFileSync(file, 'utf8')) as { id?: unknown } | null)?.id
    if (typeof id === 'string' && ID.test(id)) return id
  } catch {
    // Missing or unreadable: a new one below.
  }
  const id = Buffer.from(random(32)).toString('base64url')
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 })
  const tmp = `${file}.${Buffer.from(random(8)).toString('hex')}.tmp`
  try {
    fs.writeFileSync(tmp, JSON.stringify({ v: 1, id }), { mode: 0o600, flag: 'wx' })
    fs.renameSync(tmp, file)
  } catch {
    fs.rmSync(tmp, { force: true })
    // Not saved: this run still uses the new id; the next run makes another.
  }
  return id
}
