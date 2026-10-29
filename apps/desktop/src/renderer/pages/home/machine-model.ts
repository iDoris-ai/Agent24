// The one model behind the home-page machine figure (see MachineFigure.tsx).
//
// Following the "anatomy" figure discipline: motion, the readout caption and
// the aria text are ALL derived from `machineAt(t)` — nothing in the drawing
// keeps its own clock or its own copy of the stage list. Change a duration or
// a route here and the animation, the caption and the screen-reader text move
// together. Words live in home.i18n.ts, keyed by these stage / station ids.
//
// Coordinates are floor-grid units of the isometric drawing (x → lower-right,
// y → lower-left, z → up). The facts each stage narrates come from
// docs/ARCHITECTURE-LAYERS.md (L3 services, L5 agent24d, L7 desktop shell).

export type StationId =
  | 'entry' | 'scheduler' | 'comm' | 'daemon' | 'memory'
  | 'models' | 'gate' | 'tools' | 'store'

export type Vec3 = readonly [number, number, number]

export interface Station {
  id: StationId
  /** Callout number shown on the drawing and in the legend. */
  n: number
  /** Desktop page that shows this component, if any. */
  page?: string
  /** Floor footprint (x, y, w, d); the packet is hidden while inside it. */
  box: readonly [number, number, number, number]
  h: number
}

export const STATIONS: readonly Station[] = [
  { id: 'entry', n: 1, page: 'chat', box: [0, 5, 2, 2], h: 1.3 },
  { id: 'scheduler', n: 2, page: 'schedules', box: [0, 1, 1.6, 1.6], h: 1.1 },
  { id: 'comm', n: 3, page: 'communication', box: [0, 9.2, 1.6, 1.6], h: 0.9 },
  { id: 'daemon', n: 4, box: [4, 4.5, 3, 3], h: 1.9 },
  { id: 'memory', n: 5, page: 'memory', box: [4, 0.5, 3, 1.6], h: 2.1 },
  { id: 'models', n: 6, page: 'models', box: [9, 0.8, 2.4, 2], h: 1.6 },
  { id: 'gate', n: 7, page: 'approvals', box: [9.4, 3.7, 1.6, 0.6], h: 1.2 },
  { id: 'tools', n: 8, page: 'modules-manager', box: [9, 5, 2.4, 2], h: 1.3 },
  { id: 'store', n: 9, page: 'runs', box: [4, 9, 3, 1.8], h: 0.3 },
]

export const STATION_BY_ID = Object.fromEntries(STATIONS.map((s) => [s.id, s])) as Record<StationId, Station>

/** A run is triggered by one of three sources; successive runs rotate through them. */
export type Source = 'entry' | 'scheduler' | 'comm'
export const SOURCES: readonly Source[] = ['entry', 'scheduler', 'comm']

const Z = 0.25 // pipes run just above the floor

// Pipe routes between station faces. Reversed where a stage travels backwards.
const ROUTE_IN: Record<Source, Vec3[]> = {
  entry:     [[1, 6, Z], [2, 6, Z], [4, 6, Z], [5.5, 6, Z]],
  scheduler: [[0.8, 1.8, Z], [1.6, 1.8, Z], [3, 1.8, Z], [3, 5.2, Z], [4, 5.2, Z], [5.5, 6, Z]],
  comm:      [[0.8, 10, Z], [1.6, 10, Z], [3, 10, Z], [3, 6.8, Z], [4, 6.8, Z], [5.5, 6, Z]],
}
const ROUTE_RECALL: Vec3[] = [[5.5, 6, Z], [5.5, 4.5, Z], [5.5, 2.1, Z], [5.5, 1.3, Z], [5.5, 2.1, Z], [5.5, 4.5, Z], [5.5, 6, Z]]
const ROUTE_INFER: Vec3[] = [[5.5, 6, Z], [7, 5, Z], [8.2, 5, Z], [8.2, 2, Z], [9, 2, Z], [10.2, 1.8, Z]]
const ROUTE_TO_GATE: Vec3[] = [[10.2, 1.8, Z], [10.2, 2.8, Z], [10.2, 3.5, Z]]
const ROUTE_THROUGH_GATE: Vec3[] = [[10.2, 3.5, Z], [10.2, 4.4, Z], [10.2, 5, Z], [10.2, 6, Z]]
const ROUTE_AUDIT: Vec3[] = [[10.2, 6, Z], [9, 6.8, Z], [8.2, 6.8, Z], [8.2, 9.9, Z], [7, 9.9, Z], [5.5, 9.9, Z]]
const ROUTE_REPLY_TAIL: Vec3[] = [[5.5, 9.9, Z], [5.5, 9, Z], [5.5, 7.5, Z], [5.5, 6, Z]]

/** Every pipe drawn on the floor (deduplicated from the routes above). */
export const PIPES: readonly Vec3[][] = [
  ROUTE_IN.entry.slice(1, 3), ROUTE_IN.scheduler.slice(1, 5), ROUTE_IN.comm.slice(1, 5),
  ROUTE_RECALL.slice(1, 3), ROUTE_INFER.slice(1, 5), [[10.2, 2.8, Z], [10.2, 5, Z]],
  ROUTE_AUDIT.slice(1, 5), ROUTE_REPLY_TAIL.slice(1, 3),
]

export type StageId = 'intake' | 'run' | 'recall' | 'infer' | 'tool' | 'gate' | 'exec' | 'audit' | 'reply'

interface StageDef {
  id: StageId
  ms: number
  /** Station(s) lit while this stage runs. */
  lit: (src: Source) => StationId[]
  route: (src: Source) => Vec3[] | null
}

export const STAGES: readonly StageDef[] = [
  { id: 'intake', ms: 1800, lit: (s) => [s, 'daemon'], route: (s) => ROUTE_IN[s] },
  { id: 'run', ms: 1200, lit: () => ['daemon', 'store'], route: () => null },
  { id: 'recall', ms: 2000, lit: () => ['memory'], route: () => ROUTE_RECALL },
  { id: 'infer', ms: 2000, lit: () => ['models'], route: () => ROUTE_INFER },
  { id: 'tool', ms: 1300, lit: () => ['gate'], route: () => ROUTE_TO_GATE },
  { id: 'gate', ms: 1800, lit: () => ['gate'], route: () => [ROUTE_TO_GATE[2], ROUTE_TO_GATE[2]] },
  { id: 'exec', ms: 1600, lit: () => ['gate', 'tools'], route: () => ROUTE_THROUGH_GATE },
  { id: 'audit', ms: 1800, lit: () => ['store'], route: () => ROUTE_AUDIT },
  { id: 'reply', ms: 1800, lit: (s) => ['daemon', s], route: (s) => [...ROUTE_REPLY_TAIL, ...[...ROUTE_IN[s]].reverse()] },
]

export const STAGE_STARTS: readonly number[] = STAGES.reduce<number[]>(
  (acc, s, i) => [...acc, i === 0 ? 0 : acc[i - 1] + STAGES[i - 1].ms], [],
)
export const CYCLE_MS = STAGES.reduce((sum, s) => sum + s.ms, 0)

export interface MachineState {
  runNo: number
  source: Source
  stageIndex: number
  stage: StageId
  /** 0..1 progress through the current stage. */
  p: number
  lit: StationId[]
  /** Packet position, or null while it is inside a station. */
  packet: Vec3 | null
  /** Blocks in the audit ledger (grows by one each run's audit stage). */
  ledger: number
  /** Gate arm angle in degrees: 0 = closed, 80 = open. */
  gateDeg: number
}

function along(route: Vec3[], p: number): Vec3 {
  const lens = route.slice(1).map((b, i) => Math.hypot(b[0] - route[i][0], b[1] - route[i][1], b[2] - route[i][2]))
  const total = lens.reduce((a, b) => a + b, 0)
  let d = Math.min(Math.max(p, 0), 1) * total
  for (let i = 0; i < lens.length; i++) {
    if (d <= lens[i] || i === lens.length - 1) {
      const f = lens[i] === 0 ? 0 : Math.min(d / lens[i], 1)
      const a = route[i]
      const b = route[i + 1]
      return [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f]
    }
    d -= lens[i]
  }
  return route[route.length - 1]
}

function insideStation(v: Vec3): boolean {
  return STATIONS.some(({ id, box: [x, y, w, d] }) =>
    id !== 'gate' && v[0] > x + 0.05 && v[0] < x + w - 0.05 && v[1] > y + 0.05 && v[1] < y + d - 0.05)
}

const ease = (p: number) => p * p * (3 - 2 * p)

/** The machine at elapsed time `t` (ms). Pure; negative t clamps to 0. */
export function machineAt(t: number): MachineState {
  const tt = Math.max(0, t)
  const runNo = Math.floor(tt / CYCLE_MS)
  const inCycle = tt - runNo * CYCLE_MS
  let stageIndex = STAGES.length - 1
  for (let i = 0; i < STAGES.length; i++) {
    if (inCycle < STAGE_STARTS[i] + STAGES[i].ms) { stageIndex = i; break }
  }
  const def = STAGES[stageIndex]
  const p = Math.min(1, (inCycle - STAGE_STARTS[stageIndex]) / def.ms)
  const source = SOURCES[runNo % SOURCES.length]
  const route = def.route(source)
  const pos = route ? along(route, ease(p)) : null
  const auditIdx = STAGES.findIndex((s) => s.id === 'audit')
  const gateDeg = def.id === 'exec' ? 80 * Math.min(1, p * 3) * (p > 0.8 ? (1 - p) / 0.2 : 1) : 0
  return {
    runNo, source, stageIndex, stage: def.id, p,
    lit: def.lit(source),
    packet: pos && !insideStation(pos) ? pos : null,
    ledger: 3 + (runNo % 4) + (stageIndex > auditIdx || (stageIndex === auditIdx && p > 0.85) ? 1 : 0),
    gateDeg,
  }
}

/** Start time of the stage `delta` steps away from the one at `t` — keyboard / button stepping. */
export function stepTo(t: number, delta: number): number {
  const { runNo, stageIndex } = machineAt(t)
  const flat = runNo * STAGES.length + stageIndex + delta
  const target = Math.max(0, flat)
  const r = Math.floor(target / STAGES.length)
  return r * CYCLE_MS + STAGE_STARTS[target % STAGES.length]
}

