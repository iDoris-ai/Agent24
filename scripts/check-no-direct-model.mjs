#!/usr/bin/env node
// IC-2: chat and embeddings must not name a direct oMLX or Ollama endpoint.
// IC-4 deletes the desktop and node-daemon hits and empties that part of the
// whitelist. agent24-idoris probe code stays: it configures oMLX for iDoris
// itself (management plane), not an Agent24 model path.

import { mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
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
]);
const SOURCE_EXT = new Set(['.rs', '.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs']);

// Explicit exemptions. Do not add a file to hide a new direct model path.
const WHITELIST = new Set([
  // IC-4 deletes these desktop / node-daemon direct oMLX calls.
  'apps/desktop/src/main/omlx-kept.ts',
  'apps/desktop/src/main/omlx-quit-policy.ts',
  'apps/desktop/src/main/ipc/index.ts',
  'apps/desktop/src/renderer/App.tsx',
  'apps/desktop/src/renderer/pages/Settings.i18n.ts',
  'apps/desktop/src/renderer/pages/Chat.i18n.ts',
  'apps/desktop/src/renderer/pages/settings/ModelServiceSettings.tsx',
  'packages/node-daemon/src/llm-gateway.ts',
  // iDoris management plane: probe the local oMLX iDoris itself is configured
  // to use. Not an Agent24 chat or embedding client.
  'rust/crates/agent24-idoris/src/omlx_probe.rs',
  // IC-1 owns LaunchAgent env passthrough. A parallel slice edits this file.
  'rust/apps/agent24-cli/src/service.rs',
  // The hits are in this file's test module. A parallel slice owns the file.
  'rust/apps/agent24d/src/idoris_routes.rs',
]);

function skipped(rel) {
  const parts = rel.split(path.sep);
  if (parts.some((part) => SKIP_DIRS.has(part))) return true;
  return /test/i.test(path.basename(rel));
}

function walk(dir, hits) {
  let entries;
  try {
    entries = readdirSync(dir);
  } catch {
    return;
  }
  for (const name of entries) {
    const abs = path.join(dir, name);
    const rel = path.relative(ROOT, abs);
    if (skipped(rel) || WHITELIST.has(rel)) continue;
    let info;
    try {
      info = statSync(abs);
    } catch {
      continue;
    }
    if (info.isDirectory()) {
      if (!SKIP_DIRS.has(name)) walk(abs, hits);
      continue;
    }
    if (!SOURCE_EXT.has(path.extname(name))) continue;
    const text = readFileSync(abs, 'utf8');
    const lines = text.split(/\r?\n/);
    lines.forEach((line, index) => {
      if (NEEDLE.test(line)) hits.push(`${rel}:${index + 1}: ${line.trim()}`);
    });
  }
}

function scanTree() {
  const hits = [];
  for (const root of ROOTS) walk(path.join(ROOT, root), hits);
  return hits;
}

function selfTest() {
  const dir = mkdtempSync(path.join(tmpdir(), 'a24-no-direct-model-'));
  const file = path.join(dir, 'violation.rs');
  try {
    writeFileSync(file, 'fn bad() { let _ = "OMLX_URL"; }\n');
    const text = readFileSync(file, 'utf8');
    if (!NEEDLE.test(text)) {
      console.error('self-test: planted violation was not detected');
      process.exit(1);
    }
    console.log('self-test: planted OMLX_URL violation detected');
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
