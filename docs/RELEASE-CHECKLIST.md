# Release checklist — v0.1.0

The release actions (tag / GitHub Release / upload) are the final gate. Run
this top-to-bottom; every box must be checked before publishing.

## 1. Pre-flight (green tree)

- [ ] `main` is up to date and C1–C7 are merged.
- [ ] `pnpm install --frozen-lockfile` clean.
- [ ] `pnpm typecheck` clean.
- [ ] `pnpm test` clean (desktop + node-daemon).
- [ ] Rust: from `rust/` — `cargo fmt --check`, `cargo clippy --workspace
      --all-targets -- -D warnings`, `cargo test --workspace` all clean.
- [ ] Contract tests pass against **both** backends
      (`A24_TARGET=node` and `A24_TARGET=rust`).
- [ ] Version is `0.1.0` in: root `package.json`, `apps/desktop/package.json`,
      `packages/node-daemon/package.json`, `rust/**/Cargo.toml`.
- [ ] `CHANGELOG.md` has the `0.1.0` entry with today's date.

## 2. Build the installer (macOS dmg)

- [ ] `pnpm --filter @agent24/desktop build:mac`
      (builds the release `agent24d` + `agent24` binaries, the node daemon,
      the renderer, then packages the dmg into `apps/desktop/release/`).
- [ ] The dmg exists and the embedded backend is present at
      `Agent24.app/Contents/Resources/backend/agent24d`.

## 3. "Mother test" — the AgentStore hard gate (macOS)

Install the dmg on a clean account and verify, WITHOUT touching a terminal or
a config file:

- [ ] **≤ 60 s to first launch** — the app opens and reaches a usable screen
      within a minute of double-click.
- [ ] **No config editing** — nothing must be hand-edited to start.
- [ ] **Errors speak human** — any failure shows a plain-language message, never
      a stack trace.
- [ ] **Core features ≤ 3 steps** — from launch, each of these is reachable in
      three clicks or fewer:
  - [ ] **Chat**: send a message, get a reply (or a clear "no AI runtime"
        message if no local model is running).
  - [ ] **Schedule**: create a schedule (e.g. `every 3600s`), see its next-fire
        time, and it appears in the list.
  - [ ] **Approval**: trigger a run that needs approval (a `shell_exec` tool
        call), see the desktop notification + the Approvals page, approve it,
        and the run completes.
- [ ] **No CLI/Git/network assumptions** — a non-technical user could do all of
      the above.

## 4. Publish (release actions)

> The user authorized full automated publishing for v0.1.0. These are the exact
> commands; they are also what a human runs if doing it by hand.

- [ ] Tag: `git tag -a v0.1.0 -m "Agent24 v0.1.0" && git push origin v0.1.0`
- [ ] GitHub Release from the tag. For this first release the whole CHANGELOG
      *is* the 0.1.0 notes, so use it directly (no fragile sed range that
      could truncate):
      `gh release create v0.1.0 --title "Agent24 v0.1.0" --notes-file CHANGELOG.md`
      (Future releases: extract just their section with
      `awk '/^## \[X\.Y\.Z\]/{f=1} f&&/^## \[/&&!/X\.Y\.Z/{exit} f' CHANGELOG.md`.)
- [ ] Upload the installer + CLI as release assets:
      `gh release upload v0.1.0 apps/desktop/release/*.dmg rust/target/release/agent24 rust/target/release/agent24d`
- [ ] Verify the Release page lists the dmg + `agent24` + `agent24d`.

## Known limitations (v0.1.0)

- **Only the macOS dmg is verified** for v0.1.0. Windows (`nsis`) and Linux
  (`AppImage`) packaging IS configured — per-platform `extraResources` embed
  the native `agent24d` (Windows uses `agent24d.exe`, matched by
  `resolveRustBinary`) — but those installers are unbuilt/untested here. Each
  platform must build its own native binary (`build:rust` runs on that OS); no
  cross-compilation.
- `build:rust` and `extraResources` share Cargo's **default** target dir. If
  `CARGO_TARGET_DIR` is set in the environment, the binary lands elsewhere and
  electron-builder won't find it — unset it (or don't set it) when packaging.
