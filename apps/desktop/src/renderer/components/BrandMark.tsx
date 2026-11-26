// VI v0.2 §3 「菌丝节点」(docs/brand/VI.md): the amber core is this machine's
// agent; three hyphae reach its shells and other agents. Same geometry as
// docs/brand/src/gen-svg.py's mark (256 grid), drawn with theme variables so
// it follows dark / light. Small sizes carry no "24" inside the core.
const C = 128
const NODES = [330, 90, 210].map((deg) => {
  const a = ((deg - 90) * Math.PI) / 180
  return { x: C + 92 * Math.cos(a), y: C + 92 * Math.sin(a) }
})

export function BrandMark({ className }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 256 256" aria-hidden="true">
      {NODES.map((n, i) => (
        <line key={i} x1={C} y1={C} x2={n.x} y2={n.y} stroke="var(--weak)" strokeWidth={14} strokeLinecap="round" />
      ))}
      {NODES.map((n, i) => <circle key={i} cx={n.x} cy={n.y} r={20} fill="var(--text)" />)}
      <circle cx={C} cy={C} r={38} fill="var(--accent)" />
    </svg>
  )
}
