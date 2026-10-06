// electron-builder's extraResources entry for the Open Design on-demand
// component manifest (see package.json's `build.extraResources` and
// scripts/pack-open-design-component.mjs) points at `.open-design-component/`.
// electron-builder errors if an extraResources `from` path doesn't exist at
// all, so this guarantees the directory is there even for an ad-hoc local
// build that never ran `pnpm pack:open-design` — the app then correctly
// reports the Open Design component as `unavailable` (no manifest inside),
// rather than failing to package at all.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const scriptDir = path.dirname(fileURLToPath(import.meta.url))
const dir = path.resolve(scriptDir, '../.open-design-component')
fs.mkdirSync(dir, { recursive: true })
process.stdout.write(`Ensured ${dir} exists\n`)
