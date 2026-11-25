#!/usr/bin/env node
// IC-2: chat and embeddings must not name a direct oMLX or Ollama endpoint.
// IC-4 deletes the desktop and node-daemon hits and empties that part of the
// whitelist. iDoris owns oMLX endpoint discovery.

import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const NEEDLE = /8088|11434|OMLX_URL|OLLAMA_HOST|OLLAMA_URL/;
const ROOTS = ['rust', 'apps/desktop/src', 'packages'];
const SKIP_DIRS = new Set([
  'node_modules',
  'dist',
  'docs',
  'target',
  'fixtures',
  'tests',
  'test',
  // packages/contract-tests: the contract test harness, not shipped code.
  'contract-tests',
]);
const SOURCE_EXT = new Set(['.rs', '.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs']);

// Explicit exemptions, each capped at the number of hits it has today: a new
// direct endpoint added to an exempt file still fails. Do not raise a cap to
// hide a new direct model path.
const ALLOWED = new Map([
  // IC-4 deletes these desktop / node-daemon direct oMLX calls; omlx-kept and
  // omlx-quit-policy stay until iDoris owns the oMLX lifecycle (ID-3).
  ['apps/desktop/src/main/omlx-kept.ts', 3],
  ['apps/desktop/src/main/omlx-quit-policy.ts', 1],
  ['apps/desktop/src/main/ipc/index.ts', 6],
  ['apps/desktop/src/renderer/App.tsx', 1],
  ['apps/desktop/src/renderer/pages/Settings.i18n.ts', 3],
  ['apps/desktop/src/renderer/pages/Chat.i18n.ts', 3],
  ['apps/desktop/src/renderer/pages/settings/ModelServiceSettings.tsx', 2],
  ['packages/node-daemon/src/llm-gateway.ts', 12],
  // The names appear only in the LaunchAgent env deny list and its tests.
  ['rust/apps/agent24-cli/src/service.rs', 4],
  // The hits are in this file's test module.
  ['rust/apps/agent24d/src/idoris_routes.rs', 2],
]);

// Test sources only: test directories, `*.test.*` / `*.spec.*`, Rust
// `tests.rs` / `*_test(s).rs`. A name that merely contains "test"
// (`latest.rs`, `contest.ts`) is production code and is scanned.
function skipped(rel) {
  const parts = rel.split(path.sep);
  if (parts.some((part) => SKIP_DIRS.has(part))) return true;
  const base = path.basename(rel);
  return /\.(test|spec)\.[cm]?[jt]sx?$/.test(base) || /^tests?\.rs$/.test(base) || /_tests?\.rs$/.test(base);
}

function walk(dir, hits, root) {
  let entries;
  try {
    entries = readdirSync(dir);
  } catch {
    return;
  }
  for (const name of entries) {
    const abs = path.join(dir, name);
    const rel = path.relative(root, abs);
    if (skipped(rel)) continue;
    let info;
    try {
      info = statSync(abs);
    } catch {
      continue;
    }
    if (info.isDirectory()) {
      if (!SKIP_DIRS.has(name)) walk(abs, hits, root);
      continue;
    }
    if (!SOURCE_EXT.has(path.extname(name))) continue;
    const text = readFileSync(abs, 'utf8');
    const found = [];
    text.split(/\r?\n/).forEach((line, index) => {
      if (NEEDLE.test(line)) found.push(`${rel}:${index + 1}: ${line.trim()}`);
    });
    const allowed = ALLOWED.get(rel.split(path.sep).join('/')) ?? 0;
    if (found.length > allowed) hits.push(...found);
  }
}

function scanTree(root = ROOT) {
  const hits = [];
  for (const sub of ROOTS) walk(path.join(root, sub), hits, root);
  return hits;
}

// Runs the real traversal on a planted tree: a production file whose name
// contains "test" and an exempt file pushed over its cap must both be caught,
// and a real test file must not be.
function selfTest() {
  const dir = mkdtempSync(path.join(tmpdir(), 'a24-no-direct-model-'));
  try {
    const plant = (rel, text) => {
      const abs = path.join(dir, rel);
      mkdirSync(path.dirname(abs), { recursive: true });
      writeFileSync(abs, text);
    };
    plant('rust/crates/x/src/latest.rs', 'const URL: &str = "OMLX_URL";\n');
    plant('rust/apps/agent24-cli/src/service.rs', 'OMLX_URL\n'.repeat(5));
    plant('rust/crates/x/src/lib.test.ts', 'OMLX_URL\n');
    plant('rust/crates/x/tests/it.rs', 'OMLX_URL\n');
    const hits = scanTree(dir);
    const caught = (rel) => hits.some((hit) => hit.startsWith(rel));
    const ok = caught('rust/crates/x/src/latest.rs')
      && caught('rust/apps/agent24-cli/src/service.rs')
      && !caught('rust/crates/x/src/lib.test.ts')
      && !caught('rust/crates/x/tests/it.rs');
    if (!ok) {
      console.error('self-test: planted violations not handled as expected', hits);
      process.exit(1);
    }
    console.log('self-test: planted violations detected, test files skipped');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

if (process.argv.includes('--self-test')) selfTest();

const hits = scanTree();
if (hits.length > 0) {
  console.error(`direct model endpoint hits: ${hits.length}`);
  for (const hit of hits) console.error(hit);
  process.exit(1);
}
console.log('no direct model endpoints');
