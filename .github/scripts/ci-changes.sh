#!/usr/bin/env bash
# CI path filter (ci.yml `changes` job). Reads changed paths, one per line, on
# stdin and prints `key=true|false` lines for $GITHUB_OUTPUT:
#   rust          Rust (fmt + clippy + test) and hyphae-linux-x64
#   contract_rust Contract (agent24d)
#   typecheck     Typecheck + Test
#   contract_node Contract Tests + Codegen Drift
# rust-macos / hyphae-darwin-arm64 reuse `rust`; ci.yml adds the base-branch rule.
#
# Anything this script does not recognise runs everything: .github/**, scripts/**,
# protocol/**, any repo-root file other than *.md / LICENSE* / NOTICE / TRADEMARK*,
# and any top-level directory not listed below. An empty list also runs everything.
#
# Cross-directory reads these rules cover (keep in sync when tests start reading
# a new path outside their own tree):
#   Rust tests: docs/documenting/samples, docs/capabilities, docs/adr,
#     docs/knowledge, docs/agent/CORE-2-ENROLL-DESIGN.md,
#     docs/ARCHITECTURE-LAYERS.md (gen-layer-deps.py --check), examples/node-module,
#     packages/** and apps/<not desktop> (inbound_bypass scans them),
#     apps/desktop/{package.json,scripts/release-mac.sh,scripts/open-design-mac-release.mjs}
#     (team_id_consistency).
#   Contract tests: docs/knowledge, docs/documenting/adr, rust/apps/agent24-cli
#     (scripts/tui-coverage.mjs).
#   Desktop / packages tests: rust/apps/agent24-documents/{tests/fixtures,domain-os.yml},
#     rust/Cargo.toml, docs/capabilities, docs/adr, docs/documenting/samples, skills/**.
set -euo pipefail
echo "CI-01 反面对照：故意失败" >&2; exit 1

rust=false contract_rust=false typecheck=false contract_node=false all=false
seen=false

while IFS= read -r f; do
  [ -n "$f" ] || continue
  seen=true
  case "$f" in
    .github/* | scripts/* | protocol/*)
      all=true ;;
    */*) ;;
    *.md | LICENSE* | NOTICE | TRADEMARK*) ;;
    *) all=true ;;  # other repo-root files: package.json, pnpm-*, tsconfig*, eslint, redocly ...
  esac
  case "$f" in
    rust/*)
      rust=true contract_rust=true ;;
  esac
  case "$f" in
    rust/apps/agent24-cli/*)
      contract_node=true ;;
    rust/apps/agent24-documents/tests/fixtures/* | rust/apps/agent24-documents/domain-os.yml | rust/Cargo.toml)
      typecheck=true ;;
  esac
  case "$f" in
    apps/desktop/package.json | apps/desktop/scripts/release-mac.sh | apps/desktop/scripts/open-design-mac-release.mjs)
      rust=true contract_rust=true typecheck=true ;;
    apps/desktop/*)
      typecheck=true ;;
    apps/*)
      rust=true contract_rust=true typecheck=true ;;
    packages/*)
      rust=true contract_rust=true typecheck=true contract_node=true ;;
    examples/*)
      rust=true contract_rust=true ;;
    skills/*)
      typecheck=true ;;
    docs/documenting/samples/* | docs/capabilities/* | docs/adr/*)
      rust=true contract_rust=true typecheck=true ;;
    docs/knowledge/*)
      rust=true contract_rust=true contract_node=true ;;
    docs/documenting/adr/*)
      contract_rust=true contract_node=true ;;
    docs/agent/CORE-2-ENROLL-DESIGN.md | docs/ARCHITECTURE-LAYERS.md)
      rust=true contract_rust=true ;;
    rust/* | docs/* | eval/* | improvement/* | tools/* | vendor/* | .github/* | scripts/* | protocol/*) ;;
    */*)
      all=true ;;  # a top-level directory this filter does not know
  esac
done

if [ "$all" = true ] || [ "$seen" = false ]; then
  rust=true contract_rust=true typecheck=true contract_node=true
fi

echo "rust=$rust"
echo "contract_rust=$contract_rust"
echo "typecheck=$typecheck"
echo "contract_node=$contract_node"
