// FU-16: `cliRunner` MUST always settle.
//
// Why this is a liveness property and not a nicety: `tick()` in `main.ts` is a
// SEQUENTIAL self-rescheduling loop — it awaits `pollOnce` and only then arms
// the next `setTimeout`. A child process that never calls back leaves that
// promise pending forever, so no further poll is ever scheduled. Inbound stops
// permanently while the process stays alive, healthy-looking, and silent:
// launchd's KeepAlive sees a live process, nothing is logged, and the failure is
// invisible until someone notices the agent stopped answering. F5 is a 7-day
// unattended soak, which is precisely when a wedged child happens.
//
// MUTATION CHECK: drop `timeout` from the `execFile` options in `speaker.ts`.
//   → "rejects instead of hanging when the child never exits" times out and
//     fails, which is the real bug reproducing.

import { describe, it, expect } from 'vitest'
import { cliRunner } from './speaker.js'

describe('cliRunner liveness', () => {
  it('rejects instead of hanging when the child never exits', async () => {
    // `sleep 30` stands in for a wedged agent-speaker (hung relay socket, DNS
    // stall). Without a timeout this promise never settles and the assertion
    // below is never reached — the test fails by timing out, as it should.
    const run = cliRunner('sleep', 150)

    const started = Date.now()
    await expect(run(['30'])).rejects.toThrow(/timed out after 150ms/)
    expect(Date.now() - started).toBeLessThan(5_000)
  })

  it('names the timeout in the error — "failed: null" tells an operator nothing', async () => {
    const run = cliRunner('sleep', 100)
    await expect(run(['30'])).rejects.toThrow(/agent-speaker 30 timed out/)
  })

  it('still resolves normally well inside the timeout', async () => {
    const run = cliRunner('echo', 10_000)
    await expect(run(['{"ok":true}'])).resolves.toContain('"ok"')
  })

  it('still rejects with the real message when the binary is missing', async () => {
    const run = cliRunner('agent-speaker-does-not-exist', 10_000)
    await expect(run(['agent', 'inbox'])).rejects.toThrow(/agent-speaker agent failed:/)
  })
})
