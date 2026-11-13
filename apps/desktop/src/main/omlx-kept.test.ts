import fs from 'node:fs'
import net, { type AddressInfo } from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { KEPT_OMLX_FORMAT, KEPT_OMLX_VERSION, keptOmlxPath, readKeptOmlx, realOmlxProcessProbe, rememberKeptOmlx, stopKeptOmlx, type OmlxProcessProbe, type ProbeRead } from './omlx-kept'

// PR#1 F1 follow-up (rulings 2026-11-13-pr1-omlx-orphan-ownership 乙,
// 2026-11-13-pr1-omlx-kept-deactivate A, 2026-11-13-pr1-remaining-lows-gate-f L-b).
type Part = 'started' | 'command' | 'listens'
interface FakeProc {
  started: string
  command: string
  port: number | null
  alive: boolean
  /** A process of another user: kill(pid, 0) says EPERM. */
  other?: boolean
  diesOn?: NodeJS.Signals
  /** Parts ps/lsof cannot read (timed out). */
  unread?: Part[]
}

const read = <T,>(value: T): ProbeRead<T> => ({ ok: true, value })
const UNREAD = { ok: false } as const

function fakeProbe(procs: Map<number, FakeProc>) {
  const signals: Array<[number, NodeJS.Signals]> = []
  const live = (pid: number) => { const p = procs.get(pid); return p?.alive ? p : undefined }
  const probe: OmlxProcessProbe = {
    exists: (pid) => { const p = live(pid); return p === undefined ? 'gone' : p.other ? 'other' : 'yes' },
    started: (pid) => { const p = live(pid); return p === undefined || p.unread?.includes('started') ? UNREAD : read(p.started) },
    command: (pid) => { const p = live(pid); return p === undefined || p.unread?.includes('command') ? UNREAD : read(p.command) },
    listensOn: (pid, port) => { const p = live(pid); return p?.unread?.includes('listens') ? UNREAD : read(p !== undefined && p.port === port) },
    signal: (pid, signal) => {
      signals.push([pid, signal])
      const p = live(pid)
      if (p === undefined || p.other) return false
      if (p.diesOn === signal || signal === 'SIGKILL') p.alive = false
      return true
    },
  }
  return { probe, signals }
}

const OMLX = (o: Partial<FakeProc> = {}): FakeProc => ({ started: 'Fri Nov 13 06:20:51 2026', command: 'omlx-server', port: 8088, alive: true, diesOn: 'SIGTERM', ...o })
const TIMING = { graceMs: 30, killMs: 30 }

describe('the oMLX a quit left running (omlx-kept)', () => {
  let dir = ''
  let file = ''
  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'omlx-kept-'))
    file = keptOmlxPath(path.join(dir, 'data'))
  })
  afterEach(() => {
    fs.rmSync(dir, { recursive: true, force: true })
    vi.restoreAllMocks()
  })

  it('records its pid, port, start time and command, owner-only, and keeps it across launches', () => {
    const procs = new Map([[4100, OMLX()]])
    rememberKeptOmlx(file, { pid: 4100, port: 8088 }, fakeProbe(procs).probe)
    expect(readKeptOmlx(file)).toEqual([{ pid: 4100, port: 8088, started: 'Fri Nov 13 06:20:51 2026', command: 'omlx-server' }])
    expect(fs.statSync(file).mode & 0o777).toBe(0o600)
    const raw = JSON.parse(fs.readFileSync(file, 'utf8')) as { format: string; version: number }
    expect(raw.format).toBe(KEPT_OMLX_FORMAT)
    expect(raw.version).toBe(KEPT_OMLX_VERSION)
    // A later launch keeps another one: both stay on record.
    procs.set(4200, OMLX({ started: 'Fri Nov 13 09:00:00 2026', port: 8089 }))
    rememberKeptOmlx(file, { pid: 4200, port: 8089 }, fakeProbe(procs).probe)
    expect(readKeptOmlx(file).map((k) => k.pid)).toEqual([4100, 4200])
  })

  it('replaces a record with the same pid instead of adding a second', () => {
    const procs = new Map([[4100, OMLX()]])
    rememberKeptOmlx(file, { pid: 4100, port: 8088 }, fakeProbe(procs).probe)
    procs.set(4100, OMLX({ started: 'Sat Nov 14 08:00:00 2026' }))
    rememberKeptOmlx(file, { pid: 4100, port: 8088 }, fakeProbe(procs).probe)
    expect(readKeptOmlx(file)).toEqual([{ pid: 4100, port: 8088, started: 'Sat Nov 14 08:00:00 2026', command: 'omlx-server' }])
  })

  it('writes nothing when the identity cannot be read, and never throws', () => {
    const procs = new Map([[4100, OMLX({ alive: false })]])
    expect(() => rememberKeptOmlx(file, { pid: 4100, port: 8088 }, fakeProbe(procs).probe)).not.toThrow()
    expect(fs.existsSync(file)).toBe(false)
    // An unwritable data directory: logged, not thrown.
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    vi.spyOn(console, 'error').mockImplementation(() => {})
    const blocked = path.join(dir, 'blocked')
    fs.writeFileSync(blocked, 'a file, not a directory')
    expect(() => rememberKeptOmlx(keptOmlxPath(blocked), { pid: 4100, port: 8088 }, fakeProbe(new Map([[4100, OMLX()]])).probe)).not.toThrow()
  })

  it('writes nothing when only one part of the identity can be read', () => {
    for (const missing of ['started', 'command'] as const) {
      rememberKeptOmlx(file, { pid: 4100, port: 8088 }, fakeProbe(new Map([[4100, OMLX({ unread: [missing] })]])).probe)
      expect(fs.existsSync(file), missing).toBe(false)
    }
  })

  it('reads a missing, corrupt, foreign or malformed file as no records', () => {
    expect(readKeptOmlx(file)).toEqual([])
    fs.mkdirSync(path.dirname(file), { recursive: true })
    for (const body of [
      '{not json',
      JSON.stringify({ format: 'other', version: KEPT_OMLX_VERSION, kept: [] }),
      JSON.stringify({ format: KEPT_OMLX_FORMAT, version: KEPT_OMLX_VERSION + 1, kept: [{ pid: 1, port: 8088, started: 's', command: 'c' }] }),
      JSON.stringify({ format: KEPT_OMLX_FORMAT, version: KEPT_OMLX_VERSION, kept: [{ pid: '1', port: 8088, started: 's', command: 'c' }] }),
      JSON.stringify({ format: KEPT_OMLX_FORMAT, version: KEPT_OMLX_VERSION, kept: [{ pid: 1, port: 8088, started: 's', command: 'c' }, { pid: 2 }] }),
    ]) {
      fs.writeFileSync(file, body)
      expect(readKeptOmlx(file), body).toEqual([])
    }
  })

  it('stops a recorded oMLX whose identity still matches, then clears the record', async () => {
    const procs = new Map([[4100, OMLX()]])
    rememberKeptOmlx(file, { pid: 4100, port: 8088 }, fakeProbe(procs).probe)
    const { probe, signals } = fakeProbe(procs)
    expect(await stopKeptOmlx(file, probe, TIMING)).toEqual({ stopped: [4100], dropped: [], kept: [] })
    expect(signals).toEqual([[4100, 'SIGTERM']])
    expect(procs.get(4100)!.alive).toBe(false)
    expect(fs.existsSync(file)).toBe(false)
  })

  it('kills one still up after the grace, when it is still the same process', async () => {
    const procs = new Map([[4100, OMLX({ diesOn: undefined })]])
    rememberKeptOmlx(file, { pid: 4100, port: 8088 }, fakeProbe(procs).probe)
    const { probe, signals } = fakeProbe(procs)
    expect(await stopKeptOmlx(file, probe, TIMING)).toEqual({ stopped: [4100], dropped: [], kept: [] })
    expect(signals).toEqual([[4100, 'SIGTERM'], [4100, 'SIGKILL']])
  })

  it('does not kill after the grace when the pid now belongs to another process', async () => {
    const procs = new Map([[4100, OMLX({ diesOn: undefined })]])
    rememberKeptOmlx(file, { pid: 4100, port: 8088 }, fakeProbe(procs).probe)
    const { probe, signals } = fakeProbe(procs)
    const realSignal = probe.signal
    probe.signal = (pid, signal) => {
      const sent = realSignal(pid, signal)
      // It exits on the TERM after all, and the pid is taken by something else.
      if (signal === 'SIGTERM') procs.set(4100, OMLX({ started: 'Fri Nov 13 06:21:00 2026', command: 'other', port: null, diesOn: undefined }))
      return sent
    }
    await stopKeptOmlx(file, probe, TIMING)
    expect(signals).toEqual([[4100, 'SIGTERM']])
  })

  // The mismatches the rulings name; each stops nothing and clears the record.
  const mismatches: Array<[string, Partial<FakeProc>]> = [
    ['the pid was reused (another start time)', { started: 'Sat Nov 14 10:00:00 2026' }],
    ['the record is stale (another command on that pid)', { command: '/usr/bin/python3 other.py' }],
    ['the record is stale (no longer listening on its port)', { port: 9000 }],
    ['the process already exited on its own (ESRCH)', { alive: false }],
    ['the pid now belongs to another user (EPERM)', { other: true }],
  ]
  for (const [name, now] of mismatches) {
    it(`stops nothing when ${name}, and clears the record`, async () => {
      const procs = new Map([[4100, OMLX()]])
      rememberKeptOmlx(file, { pid: 4100, port: 8088 }, fakeProbe(procs).probe)
      procs.set(4100, OMLX(now))
      const { probe, signals } = fakeProbe(procs)
      expect(await stopKeptOmlx(file, probe, TIMING)).toEqual({ stopped: [], dropped: [4100], kept: [] })
      expect(signals).toEqual([])
      expect(fs.existsSync(file)).toBe(false)
    })
  }

  // Ruling L-b: a part that cannot be read (ps or lsof timed out) is not a
  // mismatch: nothing is signalled and the record stays for the next quit.
  for (const part of ['started', 'command', 'listens'] as const) {
    it(`signals nothing and keeps the record when the ${part} part cannot be read`, async () => {
      const procs = new Map([[4100, OMLX()]])
      rememberKeptOmlx(file, { pid: 4100, port: 8088 }, fakeProbe(procs).probe)
      procs.set(4100, OMLX({ unread: [part] }))
      const { probe, signals } = fakeProbe(procs)
      expect(await stopKeptOmlx(file, probe, TIMING)).toEqual({ stopped: [], dropped: [], kept: [4100] })
      expect(signals).toEqual([])
      expect(readKeptOmlx(file).map((k) => k.pid)).toEqual([4100])
      // The next quit, the system no longer busy: stopped then.
      procs.set(4100, OMLX())
      expect(await stopKeptOmlx(file, fakeProbe(procs).probe, TIMING)).toEqual({ stopped: [4100], dropped: [], kept: [] })
      expect(fs.existsSync(file)).toBe(false)
    })
  }

  it('does not kill, and keeps the record, when the start time cannot be read before the SIGKILL', async () => {
    const procs = new Map([[4100, OMLX({ diesOn: undefined })]])
    rememberKeptOmlx(file, { pid: 4100, port: 8088 }, fakeProbe(procs).probe)
    const { probe, signals } = fakeProbe(procs)
    const realSignal = probe.signal
    probe.signal = (pid, signal) => {
      const sent = realSignal(pid, signal)
      if (signal === 'SIGTERM') procs.get(4100)!.unread = ['started']
      return sent
    }
    expect(await stopKeptOmlx(file, probe, TIMING)).toEqual({ stopped: [4100], dropped: [], kept: [4100] })
    expect(signals).toEqual([[4100, 'SIGTERM']])
    expect(readKeptOmlx(file).map((k) => k.pid)).toEqual([4100])
  })

  it('a probe that throws for one record keeps it unsignalled and still stops the others', async () => {
    const procs = new Map([[4100, OMLX()], [4200, OMLX({ started: 'Fri Nov 13 09:00:00 2026', port: 8089 })]])
    rememberKeptOmlx(file, { pid: 4100, port: 8088 }, fakeProbe(procs).probe)
    rememberKeptOmlx(file, { pid: 4200, port: 8089 }, fakeProbe(procs).probe)
    const { probe, signals } = fakeProbe(procs)
    const realListens = probe.listensOn
    probe.listensOn = (pid, port) => { if (pid === 4100) throw new Error('lsof'); return realListens(pid, port) }
    expect(await stopKeptOmlx(file, probe, TIMING)).toEqual({ stopped: [4200], dropped: [], kept: [4100] })
    expect(signals).toEqual([[4200, 'SIGTERM']])
    expect(readKeptOmlx(file).map((k) => k.pid)).toEqual([4100])
  })

  it('counts a record whose SIGTERM failed as dropped, not stopped', async () => {
    const procs = new Map([[4100, OMLX()]])
    rememberKeptOmlx(file, { pid: 4100, port: 8088 }, fakeProbe(procs).probe)
    const { probe, signals } = fakeProbe(procs)
    probe.signal = (pid, signal) => { signals.push([pid, signal]); return false }
    expect(await stopKeptOmlx(file, probe, TIMING)).toEqual({ stopped: [], dropped: [4100], kept: [] })
    expect(signals).toEqual([[4100, 'SIGTERM']])
    expect(fs.existsSync(file)).toBe(false)
  })

  // Ruling 2026-11-13-pr1-omlx-kept-deactivate (A): the record is settled
  // before any signal, so a quit's budget cutting the wait short leaves it
  // settled; what then survives is not tried again (a known limit).
  it('settles the record before any signal, so a stop cut short leaves it settled', async () => {
    const procs = new Map([[4100, OMLX({ diesOn: undefined })], [4200, OMLX({ started: 'Fri Nov 13 09:00:00 2026', port: 8089, unread: ['listens'] })]])
    rememberKeptOmlx(file, { pid: 4100, port: 8088 }, fakeProbe(procs).probe)
    rememberKeptOmlx(file, { pid: 4200, port: 8089 }, fakeProbe(procs).probe)
    const { probe, signals } = fakeProbe(procs)
    const seen: number[][] = []
    const realSignal = probe.signal
    probe.signal = (pid, signal) => { seen.push(readKeptOmlx(file).map((k) => k.pid)); return realSignal(pid, signal) }
    const stopping = stopKeptOmlx(file, probe, { graceMs: 200, killMs: 30 })
    // Before the wait even starts: only the one that could not be read is left.
    expect(readKeptOmlx(file).map((k) => k.pid)).toEqual([4200])
    expect(signals).toEqual([[4100, 'SIGTERM']])
    await stopping
    expect(seen).toEqual([[4200], [4200]])
  })

  it('stops all matched records at once, not one after another', async () => {
    const procs = new Map([[4100, OMLX({ diesOn: undefined })], [4200, OMLX({ started: 'Fri Nov 13 09:00:00 2026', port: 8089, diesOn: undefined })]])
    rememberKeptOmlx(file, { pid: 4100, port: 8088 }, fakeProbe(procs).probe)
    rememberKeptOmlx(file, { pid: 4200, port: 8089 }, fakeProbe(procs).probe)
    const { probe, signals } = fakeProbe(procs)
    expect((await stopKeptOmlx(file, probe, { graceMs: 150, killMs: 30 })).stopped.sort()).toEqual([4100, 4200])
    // Both TERMs before any KILL: one after another would be TERM, KILL, TERM, KILL.
    expect(signals.map(([, s]) => s)).toEqual(['SIGTERM', 'SIGTERM', 'SIGKILL', 'SIGKILL'])
  })

  it('keeps at most the newest two records, and looks at no more than two', async () => {
    const procs = new Map([4100, 4200, 4300].map((pid, i) => [pid, OMLX({ started: `Fri Nov 13 0${i}:00:00 2026`, port: 8088 + i })] as const))
    for (const [i, pid] of [4100, 4200, 4300].entries()) rememberKeptOmlx(file, { pid, port: 8088 + i }, fakeProbe(procs).probe)
    expect(readKeptOmlx(file).map((k) => k.pid)).toEqual([4200, 4300])
    // A file with more (written by hand): the oldest are dropped unlooked.
    fs.writeFileSync(file, JSON.stringify({ format: KEPT_OMLX_FORMAT, version: KEPT_OMLX_VERSION, kept: [4100, 4200, 4300].map((pid, i) => ({ pid, port: 8088 + i, started: `Fri Nov 13 0${i}:00:00 2026`, command: 'omlx-server' })) }))
    const { probe, signals } = fakeProbe(procs)
    const looked: number[] = []
    const realExists = probe.exists
    probe.exists = (pid) => { looked.push(pid); return realExists(pid) }
    expect(await stopKeptOmlx(file, probe, TIMING)).toEqual({ stopped: [4200, 4300], dropped: [4100], kept: [] })
    expect(looked).not.toContain(4100)
    expect(signals.map(([pid]) => pid)).not.toContain(4100)
  })

  it('handles each record on its own: a match is stopped, a mismatch beside it is not', async () => {
    const procs = new Map([[4100, OMLX()], [4200, OMLX({ started: 'Fri Nov 13 09:00:00 2026', port: 8089 })]])
    rememberKeptOmlx(file, { pid: 4100, port: 8088 }, fakeProbe(procs).probe)
    rememberKeptOmlx(file, { pid: 4200, port: 8089 }, fakeProbe(procs).probe)
    procs.set(4100, OMLX({ started: 'Sat Nov 14 10:00:00 2026' }))
    const { probe, signals } = fakeProbe(procs)
    expect(await stopKeptOmlx(file, probe, TIMING)).toEqual({ stopped: [4200], dropped: [4100], kept: [] })
    expect(signals).toEqual([[4200, 'SIGTERM']])
  })

  it('stops nothing and removes the file when it cannot be read; never throws', async () => {
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.writeFileSync(file, '{not json')
    const { probe, signals } = fakeProbe(new Map([[4100, OMLX()]]))
    expect(await stopKeptOmlx(file, probe, TIMING)).toEqual({ stopped: [], dropped: [], kept: [] })
    expect(signals).toEqual([])
    expect(fs.existsSync(file)).toBe(false)
    // No file at all.
    expect(await stopKeptOmlx(file, probe, TIMING)).toEqual({ stopped: [], dropped: [], kept: [] })
  })
})

// The real probe (kill -0, ps, lsof), against this test process and a port it listens on.
describe.skipIf(process.platform !== 'darwin')('realOmlxProcessProbe', () => {
  it('reads a live process, the port it listens on, and tells a gone pid and a foreign one apart', async () => {
    const server = net.createServer()
    await new Promise<void>((r) => server.listen(0, '127.0.0.1', r))
    try {
      const port = (server.address() as AddressInfo).port
      const probe = realOmlxProcessProbe()
      const started = probe.started(process.pid)
      expect(started.ok && started.value).toMatch(/\d{4}$/)
      expect(probe.command(process.pid).ok).toBe(true)
      expect(probe.exists(process.pid)).toBe('yes')
      expect(probe.listensOn(process.pid, port)).toEqual({ ok: true, value: true })
      // lsof finding nothing is a value, not a failure to read.
      expect(probe.listensOn(process.pid, port === 65535 ? 65534 : port + 1)).toEqual({ ok: true, value: false })
      // The start time reads the same whatever zone and locale the app runs in.
      const saved = { TZ: process.env['TZ'], LC_ALL: process.env['LC_ALL'] }
      try {
        process.env['TZ'] = 'Asia/Tokyo'
        process.env['LC_ALL'] = 'zh_CN.UTF-8'
        expect(realOmlxProcessProbe().started(process.pid)).toEqual(started)
      } finally {
        for (const [k, v] of Object.entries(saved)) {
          if (v === undefined) delete process.env[k]
          else process.env[k] = v
        }
      }
      const gone = 2 ** 22 + 12345
      expect(probe.exists(gone)).toBe('gone')
      expect(probe.started(gone)).toEqual({ ok: false })
      expect(probe.signal(gone, 'SIGTERM')).toBe(false)
      // launchd (pid 1) is root's: EPERM, not ESRCH.
      expect(probe.exists(1)).toBe('other')
    } finally {
      await new Promise<void>((r) => server.close(() => r()))
    }
  })
})
