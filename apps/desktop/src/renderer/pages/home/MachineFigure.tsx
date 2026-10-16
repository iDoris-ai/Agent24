// Isometric drawing of the Agent24 machine. Stateless: everything that moves
// is read off the `MachineState` handed in (see machine-model.ts), so this
// file only knows geometry. Four shading tones give volume; the one accent
// colour marks whatever the current stage is touching.

import type { ReactNode } from 'react'
import {
  PIPES, STATIONS, STAGES, STATION_BY_ID,
  type MachineState, type Station, type StationId, type Vec3,
} from './machine-model'

const U = 20
const C = Math.cos(Math.PI / 6)

function P(x: number, y: number, z = 0): [number, number] {
  return [(x - y) * C * U, (x + y) * 0.5 * U - z * U]
}
function pts(...ps: Vec3[]): string {
  return ps.map(([x, y, z]) => P(x, y, z).map((n) => n.toFixed(1)).join(',')).join(' ')
}

/** A solid block: right (+x) face, left (+y) face, top. */
function Block(props: {
  x: number; y: number; z?: number; w: number; d: number; h: number
  className?: string; leftFill?: string
}) {
  const { x, y, z = 0, w, d, h, className, leftFill } = props
  const X = x + w, Y = y + d, Zt = z + h
  return (
    <g className={className}>
      <polygon className="mf-r" points={pts([X, y, z], [X, Y, z], [X, Y, Zt], [X, y, Zt])} />
      <polygon className="mf-l" style={leftFill ? { fill: leftFill } : undefined}
        points={pts([x, Y, z], [X, Y, z], [X, Y, Zt], [x, Y, Zt])} />
      <polygon className="mf-t" points={pts([x, y, Zt], [X, y, Zt], [X, Y, Zt], [x, Y, Zt])} />
    </g>
  )
}

/** A circle lying flat at height z, as a polygon (iso ellipse). */
function flatCircle(cx: number, cy: number, z: number, r: number, n = 28): string {
  const ps: Vec3[] = []
  for (let i = 0; i < n; i++) {
    const a = (i / n) * Math.PI * 2
    ps.push([cx + r * Math.cos(a), cy + r * Math.sin(a), z])
  }
  return pts(...ps)
}

function Line({ a, b, className }: { a: Vec3; b: Vec3; className: string }) {
  const [x1, y1] = P(...a)
  const [x2, y2] = P(...b)
  return <line className={className} x1={x1} y1={y1} x2={x2} y2={y2} />
}

const wheelDeg = (s: MachineState) => (s.runNo * STAGES.length + s.stageIndex + s.p) * (360 / STAGES.length)

function StationArt({ st, s }: { st: Station; s: MachineState }): ReactNode {
  const [x, y, w, d] = st.box
  const lit = s.lit.includes(st.id)
  switch (st.id) {
    case 'entry':
      return (
        <>
          <Block x={x} y={y} w={w} d={d} h={st.h} />
          {/* monitor: its +y face is the screen */}
          <Block x={x + 0.3} y={y + 0.9} z={st.h} w={1.4} d={0.15} h={1.0} className="mf-screen" />
        </>
      )
    case 'scheduler': {
      const cx = x + w / 2, cy = y + d / 2, z = st.h
      const a = (s.runNo * STAGES.length + s.stageIndex + s.p) * 0.7
      return (
        <>
          <Block x={x} y={y} w={w} d={d} h={st.h} />
          <polygon className="mf-dial" points={flatCircle(cx, cy, z + 0.01, 0.62)} />
          <Line className="mf-hand" a={[cx, cy, z + 0.02]} b={[cx + 0.5 * Math.cos(a), cy + 0.5 * Math.sin(a), z + 0.02]} />
          <Line className="mf-hand mf-hand-short" a={[cx, cy, z + 0.02]} b={[cx + 0.3 * Math.cos(a / 12), cy + 0.3 * Math.sin(a / 12), z + 0.02]} />
        </>
      )
    }
    case 'comm': {
      const cx = x + w / 2, cy = y + d / 2
      const top = P(cx, cy, st.h + 1.6)
      const ring = (s.p * 2) % 1
      return (
        <>
          <Block x={x} y={y} w={w} d={d} h={st.h} />
          <Line className="mf-mast" a={[cx, cy, st.h]} b={[cx, cy, st.h + 1.6]} />
          <circle className="mf-ball" cx={top[0]} cy={top[1]} r={3.5} />
          {lit && [0, 0.5].map((off) => {
            const k = (ring + off) % 1
            return <circle key={off} className="mf-wave" cx={top[0]} cy={top[1]} r={5 + k * 18} style={{ opacity: 1 - k }} />
          })}
        </>
      )
    }
    case 'daemon': {
      const cx = x + w / 2, cy = y + d / 2, z = st.h + 0.15
      const a = (wheelDeg(s) * Math.PI) / 180
      const lamp = P(x + w - 0.3, y + 0.3, st.h + 0.3)
      return (
        <>
          <Block x={x} y={y} w={w} d={d} h={st.h} />
          <Block x={cx - 0.25} y={cy - 0.25} z={st.h} w={0.5} d={0.5} h={0.15} className="mf-plain" />
          <polygon className="mf-wheel" points={flatCircle(cx, cy, z, 1.15)} />
          <polygon className="mf-wheel-inner" points={flatCircle(cx, cy, z, 0.95)} />
          {[0, 1, 2].map((k) => {
            const ak = a + (k * Math.PI * 2) / 3
            return <Line key={k} className="mf-spoke" a={[cx, cy, z]} b={[cx + 1.05 * Math.cos(ak), cy + 1.05 * Math.sin(ak), z]} />
          })}
          <circle className="mf-hub" cx={P(cx, cy, z)[0]} cy={P(cx, cy, z)[1]} r={3} />
          <circle className="mf-lamp" cx={lamp[0]} cy={lamp[1]} r={3.2} />
        </>
      )
    }
    case 'memory': {
      const out = s.stage === 'recall' ? 0.7 * Math.sin(s.p * Math.PI) : 0
      const Y = y + d
      return (
        <>
          <Block x={x} y={y} w={w} d={d} h={st.h} />
          {[0.25, 0.9, 1.55].map((z0, i) => {
            const slide = i === 1 ? out : 0
            return (
              <Block key={z0} x={x + 0.25} y={Y - 0.02} z={z0} w={w - 0.5} d={0.08 + slide} h={0.5}
                className={`mf-drawer${i === 1 && slide > 0.01 ? ' mf-lit' : ''}`} />
            )
          })}
        </>
      )
    }
    case 'models': {
      const hot = s.stage === 'infer'
      const vent = P(x + 0.7, y + 0.7, st.h + 1.0)
      return (
        <>
          <Block x={x} y={y} w={w} d={d} h={st.h} />
          <Block x={x + 0.3} y={y + 0.3} z={st.h} w={0.8} d={0.8} h={0.9} className={hot ? 'mf-lit' : 'mf-plain'} />
          <Block x={x + 1.4} y={y + 0.3} z={st.h} w={0.7} d={0.7} h={0.5} className="mf-plain mf-dim" />
          {hot && [0, 0.33, 0.66].map((off) => {
            const k = (s.p * 3 + off) % 1
            return <circle key={off} className="mf-heat" cx={vent[0] + Math.sin(k * 6) * 2} cy={vent[1] - k * 22} r={2 + k * 2} style={{ opacity: 1 - k }} />
          })}
        </>
      )
    }
    case 'gate': {
      const a = (s.gateDeg * Math.PI) / 180
      const pivot: Vec3 = [x + 0.1, y + 0.3, 1.0]
      const tip: Vec3 = [pivot[0] + 1.4 * Math.cos(a), pivot[1], pivot[2] + 1.4 * Math.sin(a)]
      const lamp = P(x + w - 0.1, y + 0.3, st.h + 0.15)
      const lampCls = s.stage === 'gate' ? 'mf-lamp-wait' : s.stage === 'exec' ? 'mf-lamp-go' : 'mf-lamp-idle'
      return (
        <>
          <Block x={x} y={y + 0.2} w={0.2} d={0.2} h={st.h} />
          <Block x={x + w - 0.2} y={y + 0.2} w={0.2} d={0.2} h={st.h} />
          <Line className="mf-arm-base" a={pivot} b={tip} />
          <Line className="mf-arm" a={pivot} b={tip} />
          <circle className={lampCls} cx={lamp[0]} cy={lamp[1]} r={3.2} />
        </>
      )
    }
    case 'tools': {
      const stamping = s.stage === 'exec' && s.p > 0.4
      const lift = stamping ? 0.1 + 0.55 * Math.abs(Math.cos((s.p - 0.4) * Math.PI * 3)) : 0.65
      const hx = x + 0.8, hy = y + 0.6
      return (
        <>
          <Block x={x} y={y} w={w} d={d} h={st.h} />
          <Line className="mf-rod" a={[hx + 0.4, hy + 0.4, st.h + lift + 0.4]} b={[hx + 0.4, hy + 0.4, st.h + 2.2]} />
          <Block x={hx - 0.3} y={hy + 0.3} z={st.h + 2.2} w={1.4} d={0.2} h={0.2} className="mf-plain" />
          <Block x={hx} y={hy} z={st.h + lift} w={0.8} d={0.8} h={0.4} className={stamping ? 'mf-lit' : 'mf-plain'} />
        </>
      )
    }
    case 'store': {
      const n = s.ledger
      const dropping = s.stage === 'audit' && s.p > 0.55
      return (
        <>
          <Block x={x} y={y} w={w} d={d} h={st.h} className="mf-plain" />
          {Array.from({ length: n }, (_, i) => {
            const last = i === n - 1
            const drop = last && dropping ? (1 - Math.min(1, (s.p - 0.55) / 0.3)) * 1.6 : 0
            return (
              <Block key={i} x={x + 0.35} y={y + 0.25} z={st.h + i * 0.27 + drop} w={w - 0.7} d={d - 0.5} h={0.22}
                className={`mf-slab${last && s.stage === 'audit' ? ' mf-lit' : ''}`} />
            )
          })}
        </>
      )
    }
  }
}

const ORDER = [...STATIONS].sort((a, b) => (a.box[0] + a.box[2] / 2 + a.box[1] + a.box[3] / 2) - (b.box[0] + b.box[2] / 2 + b.box[1] + b.box[3] / 2))

export interface MachineFigureProps {
  state: MachineState
  status: 'online' | 'offline' | 'checking'
  focus: StationId | null
  onFocus: (id: StationId | null) => void
  onPick: (id: StationId) => void
  /** Accessible name of the drawing, in the current UI language. */
  label: string
}

export function MachineFigure({ state: s, status, focus, onFocus, onPick, label }: MachineFigureProps) {
  const running = status === 'online'
  const packetDepth = s.packet ? s.packet[0] + s.packet[1] : -1
  const items: { depth: number; node: ReactNode }[] = ORDER.map((st) => ({
    depth: st.box[0] + st.box[2] / 2 + st.box[1] + st.box[3] / 2,
    node: (
      <g
        key={st.id}
        className={`mf-station${running && s.lit.includes(st.id) ? ' is-lit' : ''}${focus === st.id ? ' is-focus' : ''}`}
        data-station={st.id}
        onMouseEnter={() => onFocus(st.id)}
        onMouseLeave={() => onFocus(null)}
        onClick={() => onPick(st.id)}
      >
        <StationArt st={st} s={s} />
      </g>
    ),
  }))
  if (running && s.packet) {
    const [px, py] = s.packet
    items.push({
      depth: packetDepth,
      node: (
        <g key="packet" className="mf-packet">
          <circle className="mf-packet-glow" cx={P(px, py, 0.5)[0]} cy={P(px, py, 0.5)[1]} r={9} />
          <Block x={px - 0.22} y={py - 0.22} z={0.2} w={0.44} d={0.44} h={0.44} className="mf-lit" />
        </g>
      ),
    })
  }
  items.sort((a, b) => a.depth - b.depth)

  return (
    <svg className={`mf-svg mf-${status}`} viewBox="-240 -95 470 345" role="img"
      aria-label={label}>
      <polygon className="mf-floor" points={pts([-0.6, -0.4, 0], [12.2, -0.4, 0], [12.2, 11.4, 0], [-0.6, 11.4, 0])} />
      {Array.from({ length: 12 }, (_, i) => (
        <g key={i} className="mf-grid">
          <Line className="mf-grid" a={[i, -0.4, 0]} b={[i, 11.4, 0]} />
          <Line className="mf-grid" a={[-0.6, i, 0]} b={[12.2, i, 0]} />
        </g>
      ))}
      {PIPES.map((route, i) => (
        <g key={i}>
          <polyline className="mf-pipe" points={pts(...route)} />
          <polyline className="mf-pipe-core" points={pts(...route)} />
        </g>
      ))}
      {items.map((it) => it.node)}
      {STATIONS.map((st) => {
        const [x, y, w, d] = st.box
        const [cx, cy] = P(x + w / 2, y + d / 2, st.h + (st.id === 'tools' ? 3.1 : st.id === 'comm' ? 2.3 : 1.7))
        const lit = running && s.lit.includes(st.id)
        return (
          <g key={st.id} className={`mf-callout${lit ? ' is-lit' : ''}${focus === st.id ? ' is-focus' : ''}`}
            onMouseEnter={() => onFocus(st.id)} onMouseLeave={() => onFocus(null)} onClick={() => onPick(st.id)}>
            <circle cx={cx} cy={cy} r={8} />
            <text x={cx} y={cy + 3.5} textAnchor="middle">{STATION_BY_ID[st.id].n}</text>
          </g>
        )
      })}
    </svg>
  )
}
