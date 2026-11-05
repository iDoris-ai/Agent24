import assert from 'node:assert/strict'
import fs from 'node:fs'
import test from 'node:test'
import os from 'node:os'
import path from 'node:path'
import { execFileSync } from 'node:child_process'

const desktop = new URL('../', import.meta.url)
const read = (file) => fs.readFileSync(new URL(file, desktop), 'utf8')

test('all release and runtime consumers use the single pin', async (t) => {
  const pin = JSON.parse(read('src/shared/open-design.pin.json'))
  assert.match(pin.version, /^\d+\.\d+\.\d+$/)
  assert.match(pin.sha, /^[a-f0-9]{40}$/)
  for (const script of ['pack-open-design-component', 'prepare-open-design-linux-resources']) {
    const consumer = await import(`./${script}.mjs`)
    assert.equal(consumer.OPEN_DESIGN_PIN_VERSION, pin.version)
    assert.equal(consumer.OPEN_DESIGN_SHA, pin.sha)
  }
  const runtime = read('src/main/creative-serve-web.ts')
  assert.match(runtime, /import openDesignPin from '\.\.\/shared\/open-design.pin.json'/)
  assert.match(runtime, /OPEN_DESIGN_PIN_VERSION = openDesignPin.version/)
  const stage = read('scripts/stage-open-design-resources.mjs')
  assert.match(stage, /OPEN_DESIGN_PIN_VERSION.*from '\.\/pack-open-design-component.mjs'/)
  const workflow = read('../../.github/workflows/release-desktop.yml')
  assert.match(workflow, /ref: \$\{\{ env.OPEN_DESIGN_SHA \}\}/)
  assert.equal((workflow.match(/= "\$OPEN_DESIGN_PIN_VERSION"/g) ?? []).length, 2)
  // Execute the actual workflow loader, so changes to its path/keys are checked.
  const loader = workflow.match(/- name: Load Open Design pin\n\s+shell: bash\n\s+run: \|\n((?: {10}.*\n)+)/)?.[1]
  assert.ok(loader, 'workflow must load the pin before the fork checkout')
  assert.ok(workflow.indexOf(loader) < workflow.indexOf('repository: iDoris-ai/open-design-agent24'))
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'od-pin-'))
  t.after(() => fs.rmSync(tmp, { recursive: true, force: true }))
  const envFile = path.join(tmp, 'github-env')
  execFileSync('bash', ['-eu', '-c', loader], {
    cwd: new URL('../../', desktop), env: { ...process.env, GITHUB_ENV: envFile }, encoding: 'utf8',
  })
  assert.equal(fs.readFileSync(envFile, 'utf8'), `OPEN_DESIGN_SHA=${pin.sha}\nOPEN_DESIGN_PIN_VERSION=${pin.version}\n`)
})
