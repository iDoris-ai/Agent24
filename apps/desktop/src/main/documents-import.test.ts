// The bridge to `agent24 documents import`: what main sends the CLI and how
// it reads the CLI's answer. The import itself (chunks, keys, retries, the
// upload chain) is the CLI's, tested in rust/crates/agent24-documents-client.

import { EventEmitter } from 'node:events'
import { PassThrough } from 'node:stream'
import type { ChildProcess } from 'node:child_process'
import { describe, expect, it } from 'vitest'
import { importFile, type Spawner } from './documents-import'

const EP = { port: 4321, token: 'tok-secret' }
const JOB = { job_id: 'job_01K74Z3QJ8V5N2W9RTX6YB4MCD', status: 'queued' }

interface Fake {
  spawn: Spawner
  calls: { bin: string; args: string[] }[]
  stdin: () => string
  killed: () => boolean
}

/** A CLI that writes `lines` (raw) to stdout, then exits; or emits `error`. */
function fakeCli(lines: string[], opts: { error?: boolean; stderr?: string } = {}): Fake {
  const calls: Fake['calls'] = []
  let input = ''
  let killed = false
  const spawn: Spawner = (bin, args) => {
    calls.push({ bin, args })
    const child = new EventEmitter() as ChildProcess & EventEmitter
    const stdin = new PassThrough()
    const stdout = new PassThrough()
    const stderr = new PassThrough()
    Object.assign(child, { stdin, stdout, stderr, kill: () => ((killed = true), true) })
    stdin.on('data', (c: Buffer) => (input += c.toString()))
    stdin.on('end', () => {
      if (opts.error) {
        child.emit('error', new Error('spawn agent24 ENOENT /secret/path'))
        child.emit('close', null)
        return
      }
      if (opts.stderr) stderr.write(opts.stderr)
      for (const line of lines) stdout.write(line)
      stdout.end()
      stdout.on('end', () => setImmediate(() => child.emit('close', 0)))
    })
    return child
  }
  return { spawn, calls, stdin: () => input, killed: () => killed }
}

const result = (v: object): string => `${JSON.stringify({ type: 'result', ...v })}\n`
const progress = (sent: number, total: number): string => `${JSON.stringify({ type: 'progress', sent, total })}\n`

describe('importFile through agent24 documents import', () => {
  it('runs the CLI with the endpoint, token and path on stdin, none of them in argv', async () => {
    const cli = fakeCli([result({ ok: true, status: 202, data: JOB })])
    const res = await importFile('/Users/me/私密/a b.pdf', () => EP, () => {}, cli.spawn, '/app/agent24')
    expect(res).toEqual({ ok: true, status: 202, data: JOB })
    expect(cli.calls).toEqual([{ bin: '/app/agent24', args: ['documents', 'import', '--host-stdin', '--json'] }])
    expect(JSON.parse(cli.stdin())).toEqual({ base: 'http://127.0.0.1:4321', token: 'tok-secret', path: '/Users/me/私密/a b.pdf' })
  })

  it('passes progress on, and the error envelope as it is, hint included', async () => {
    const seen: unknown[] = []
    const error = { code: 'unsupported_format', message: 'slice 1 imports PDF, JPEG and PNG', hint: 'pick a PDF' }
    const cli = fakeCli([progress(0, 10), progress(10, 10), result({ ok: false, status: 422, error })])
    const res = await importFile('/a.txt', () => EP, (p) => seen.push(p), cli.spawn, 'agent24')
    expect(seen).toEqual([{ sent: 0, total: 10 }, { sent: 10, total: 10 }])
    expect(res).toEqual({ ok: false, status: 422, error })
  })

  it('reads lines split across chunks, a last line without a newline, and skips what is not its JSON', async () => {
    const line = result({ ok: true, status: 200, data: JOB })
    const seen: unknown[] = []
    const cli = fakeCli(['not json\n', '{"type":"progress","sent":"x","total":1}\n', line.slice(0, 9), line.slice(9).trimEnd()])
    expect(await importFile('/a.pdf', () => EP, (p) => seen.push(p), cli.spawn, 'agent24')).toEqual({ ok: true, status: 200, data: JOB })
    expect(seen).toEqual([])
  })

  it('no daemon yet is backend_not_ready, and the CLI is not run', async () => {
    const cli = fakeCli([])
    const res = await importFile('/a.pdf', () => null, () => {}, cli.spawn, 'agent24')
    expect(res).toEqual({ ok: false, status: 503, error: { code: 'backend_not_ready', message: 'Backend daemon has not announced readiness yet' } })
    expect(cli.calls).toEqual([])
  })

  it('a CLI that cannot start is backend_unreachable, without the error text', async () => {
    const cli = fakeCli([], { error: true })
    const res = await importFile('/a.pdf', () => EP, () => {}, cli.spawn, 'agent24')
    expect(res).toEqual({ ok: false, status: 503, error: { code: 'backend_unreachable', message: 'the documents tool could not be started' } })
    const throwing: Spawner = () => {
      throw new Error('EACCES /secret/path')
    }
    expect(await importFile('/a.pdf', () => EP, () => {}, throwing, 'agent24')).toEqual(res)
  })

  it('an exit with no result, or a result that does not fit, is invalid_response; stderr is never shown', async () => {
    const none = await importFile('/a.pdf', () => EP, () => {}, fakeCli([progress(0, 5)], { stderr: '/Users/me/a.pdf' }).spawn, 'agent24')
    expect(none).toEqual({ ok: false, status: 502, error: { code: 'invalid_response', message: 'the documents tool ended without a result' } })
    for (const bad of [{ ok: true, status: 200 }, { ok: false, status: 409, error: { code: 1 } }, { ok: false, status: 409 }]) {
      const res = await importFile('/a.pdf', () => EP, () => {}, fakeCli([result(bad)]).spawn, 'agent24')
      expect(res).toEqual({ ok: false, status: 502, error: { code: 'invalid_response', message: 'the documents tool gave a result that does not fit' } })
    }
  })

  it('a line past the bound stops the CLI and answers invalid_response, ended or not', async () => {
    const long = result({ ok: true, status: 200, data: { ...JOB, pad: 'x'.repeat(4 * 1024 * 1024) } })
    for (const lines of [['x'.repeat(4 * 1024 * 1024 + 1)], [long], ['x'.repeat(4 * 1024 * 1024 - 1), 'xx\n' + result({ ok: true, status: 200, data: JOB })]]) {
      const cli = fakeCli(lines)
      const res = await importFile('/a.pdf', () => EP, () => {}, cli.spawn, 'agent24')
      expect(res).toEqual({ ok: false, status: 502, error: { code: 'invalid_response', message: 'the documents tool wrote a line too long' } })
      expect(cli.killed()).toBe(true)
    }
  })
})
