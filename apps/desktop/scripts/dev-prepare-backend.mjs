// Dev backend prep: build the Rust debug daemon ONLY when it will actually be
// used. Rust is the default backend (v0.1.0), but `AGENT24_BACKEND=node` is a
// legacy opt-in that never touches the Rust binary — so in node mode we skip
// the cargo build entirely, letting a node-only dev environment run `pnpm dev`
// without the Rust toolchain installed (review C8).
import { execFileSync } from 'node:child_process'

const backend = process.env.AGENT24_BACKEND === 'node' ? 'node' : 'rust'

if (backend === 'node') {
  console.log('[dev] AGENT24_BACKEND=node — skipping the Rust daemon build')
  process.exit(0)
}

console.log('[dev] building the Rust debug agent24d…')
try {
  execFileSync(
    'cargo',
    ['build', '-p', 'agent24d', '--manifest-path', '../../rust/Cargo.toml'],
    { stdio: 'inherit' },
  )
} catch {
  console.error(
    '[dev] cargo build failed. For node-only dev without the Rust toolchain, run: AGENT24_BACKEND=node pnpm dev',
  )
  process.exit(1)
}
