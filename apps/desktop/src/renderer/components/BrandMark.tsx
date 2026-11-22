// VI v0.1 §3 「昼夜环」(docs/brand/VI.md): 24 hour ticks, the 0/6/12/18 ones
// longer, the top one lit; the core dot is the daemon. Same geometry as
// docs/brand/src/gen-svg.py's mark (256 grid), drawn with theme variables so
// it follows dark / light without a second asset.
const C = 128
const R = 114

const TICKS = Array.from({ length: 24 }, (_, i) => {
  const a = (i / 24) * 2 * Math.PI - Math.PI / 2
  const lit = i === 0
  const major = i % 6 === 0
  const r = R - (major ? 40 : 14)
  return {
    x1: C + r * Math.cos(a), y1: C + r * Math.sin(a),
    x2: C + R * Math.cos(a), y2: C + R * Math.sin(a),
    width: lit ? 18 : major ? 12 : 9,
    stroke: lit ? 'var(--accent)' : 'var(--weak)',
  }
})

export function BrandMark({ className }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 256 256" aria-hidden="true">
      {TICKS.map((t, i) => (
        <line key={i} x1={t.x1} y1={t.y1} x2={t.x2} y2={t.y2} stroke={t.stroke} strokeWidth={t.width} strokeLinecap="round" />
      ))}
      <circle cx={C} cy={C} r={22} fill="var(--text)" />
    </svg>
  )
}
