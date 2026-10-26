#!/usr/bin/env node
// TUI-1 / ADR-037: CLI/TUI must cover every daemon operation in protocol/openapi.yaml,
// or the operation must be listed in protocol/tui-coverage-exemptions.yaml.
//
// "Covered" criterion: the CLI sources (rust/apps/agent24-cli/src, non-test, non-comment code)
// contain a request to `/api/v1/<path>` whose HTTP method (the nearest preceding
// .get/.post/.put/.patch/.delete( or Method::X) matches an openapi operation. Path params are
// normalised ({id} / {name} / {} -> {}). Text matching is inherently approximate, so the
// positive control lives in `--self-test` (delete a call -> must go red; fake op -> red; exempt -> green).
//
// Usage: node scripts/tui-coverage.mjs [--openapi F] [--exemptions F] [--cli-dir D] [--json] | --self-test
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const METHODS = ['get', 'post', 'put', 'patch', 'delete'];
const CATEGORIES = ['visual', 'desktop-internal', 'planned-tui-2', 'planned-tui-3'];
const norm = (p) => p.replace(/\{[^}]*\}/g, '{}').replace(/\/+$/, '');

export function parseOpenapi(text) {
  const ops = [];
  let inPaths = false, curPath = null, curMethod = null;
  for (const line of text.split('\n')) {
    if (/^paths:/.test(line)) { inPaths = true; continue; }
    if (inPaths && /^\S/.test(line)) inPaths = false;
    if (!inPaths) continue;
    let m;
    if ((m = line.match(/^  (\/\S*):\s*$/))) { curPath = m[1]; curMethod = null; }
    else if ((m = line.match(/^    (get|post|put|patch|delete):\s*$/))) curMethod = m[1];
    else if ((m = line.match(/^      operationId:\s*(\S+)/)) && curPath && curMethod)
      ops.push({ id: m[1], method: curMethod, path: norm(curPath) });
  }
  return ops;
}

export function parseExemptions(text) {
  const out = [];
  let cur = null;
  for (const raw of text.split('\n')) {
    const line = raw.replace(/\s+#.*$/, '');
    let m;
    if ((m = line.match(/^\s*- operationId:\s*(\S+)/))) { cur = { id: m[1] }; out.push(cur); }
    else if (cur && (m = line.match(/^\s+(category|fallback):\s*(.+?)\s*$/)))
      cur[m[1]] = m[2].replace(/^["']|["']$/g, '');
  }
  return out;
}

// Strip comments and #[cfg(test)] tails so test-only HTTP calls don't count as coverage.
export function cliCode(src) {
  const cut = src.search(/#\[cfg\(test\)\]/);
  if (cut >= 0) src = src.slice(0, cut);
  return src.split('\n').filter((l) => !/^\s*\/\//.test(l)).join('\n');
}

export function cliCalls(files) {
  const calls = new Set();
  for (const code of files.map(cliCode)) {
    const re = /["`]((?:[^"`\\]|\\.)*?)\/api\/v1(\/[^"`?\s]*)/g;
    let m;
    while ((m = re.exec(code))) {
      const before = code.slice(Math.max(0, m.index - 220), m.index);
      const hits = [...before.matchAll(/\.(get|post|put|patch|delete)\(|Method::(GET|POST|PUT|PATCH|DELETE)/g)];
      const last = hits.at(-1);
      if (!last) continue;
      calls.add(`${(last[1] || last[2]).toLowerCase()} ${norm(m[2])}`);
    }
  }
  return calls;
}

export function check({ openapi, exemptions, cliFiles }) {
  const ops = parseOpenapi(openapi);
  const ex = parseExemptions(exemptions);
  const calls = cliCalls(cliFiles);
  const errors = [];
  const ids = new Set(ops.map((o) => o.id));
  for (const e of ex) {
    if (!ids.has(e.id)) errors.push(`exemption for unknown operationId: ${e.id}`);
    if (!CATEGORIES.includes(e.category)) errors.push(`exemption ${e.id}: category must be one of ${CATEGORIES.join('|')}`);
    if (!e.fallback) errors.push(`exemption ${e.id}: missing fallback`);
  }
  const covered = ops.filter((o) => calls.has(`${o.method} ${o.path}`));
  const coveredIds = new Set(covered.map((o) => o.id));
  const exIds = new Set(ex.map((e) => e.id));
  for (const e of ex) if (coveredIds.has(e.id)) errors.push(`stale exemption (already covered by CLI): ${e.id}`);
  const missing = ops.filter((o) => !coveredIds.has(o.id) && !exIds.has(o.id));
  for (const o of missing) errors.push(`uncovered: ${o.id} (${o.method.toUpperCase()} ${o.path}) — add a CLI call or an exemption`);
  return { total: ops.length, covered: covered.length, exempt: ex.filter((e) => ids.has(e.id) && !coveredIds.has(e.id)).length, missing, errors, coveredIds, ops };
}

function loadCli(dir) {
  const out = [];
  (function walk(d) {
    for (const f of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, f.name);
      if (f.isDirectory()) walk(p); else if (p.endsWith('.rs')) out.push(fs.readFileSync(p, 'utf8'));
    }
  })(dir);
  return out;
}

function report(r) {
  console.log(`TUI coverage: ${r.covered}/${r.total} operations covered by CLI, ${r.exempt} exempted, ${r.missing.length} unaccounted`);
  for (const e of r.errors) console.error(`ERROR ${e}`);
}

function selfTest() {
  const openapi = fs.readFileSync(path.join(root, 'protocol/openapi.yaml'), 'utf8');
  const exemptions = fs.readFileSync(path.join(root, 'protocol/tui-coverage-exemptions.yaml'), 'utf8');
  const cliFiles = loadCli(path.join(root, 'rust/apps/agent24-cli/src'));
  let fail = 0;
  const expect = (name, cond) => { console.log(`${cond ? 'ok  ' : 'FAIL'} ${name}`); if (!cond) fail++; };
  const base = check({ openapi, exemptions, cliFiles });
  expect('real repo is green', base.errors.length === 0);
  // positive control: delete the CLI's GET /health call(s) and the checker must go red for getHealth
  const mutated = cliFiles.map((s) => s.replace(/\/api\/v1\/health/g, '/api/v1/healthz'));
  const m1 = check({ openapi, exemptions, cliFiles: mutated });
  expect('removing /health call turns red (getHealth uncovered)', m1.errors.some((e) => e.includes('uncovered: getHealth')));
  // method sensitivity: GET /attached -> POST-only must flag listAttachedModules
  const m2 = cliFiles.map((s) => s.replace(/client\(\)\.get\(format!\("\{\}\/api\/v1\/attached"/g, 'client().head(format!("{}/api/v1/attached"'));
  const m2r = check({ openapi, exemptions, cliFiles: m2 });
  expect('method mismatch turns red (listAttachedModules)', m2r.errors.some((e) => e.includes('uncovered: listAttachedModules')));
  // new operation: red, then exempt -> green
  const fake = openapi.replace(/^  \/health:/m, '  /brand-new:\n    get:\n      operationId: brandNewOp\n      responses: {}\n  /health:');
  const f1 = check({ openapi: fake, exemptions, cliFiles });
  expect('new operation without CLI/exemption is red', f1.errors.some((e) => e.includes('uncovered: brandNewOp')) && f1.total === base.total + 1);
  const f2 = check({ openapi: fake, exemptions: exemptions + '\n  - operationId: brandNewOp\n    category: visual\n    fallback: "n/a"\n', cliFiles });
  expect('adding it to exemptions turns green', f2.errors.length === 0);
  // hygiene
  const f3 = check({ openapi, exemptions: exemptions + '\n  - operationId: noSuchOp\n    category: visual\n    fallback: x\n', cliFiles });
  expect('unknown exemption id is red', f3.errors.some((e) => e.includes('unknown operationId')));
  const f4 = check({ openapi, exemptions: exemptions + '\n  - operationId: getHealth\n    category: visual\n    fallback: x\n', cliFiles });
  expect('stale exemption (already covered) is red', f4.errors.some((e) => e.includes('stale exemption')));
  process.exit(fail ? 1 : 0);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const a = process.argv.slice(2);
  const opt = (n, d) => (a.includes(n) ? a[a.indexOf(n) + 1] : d);
  if (a.includes('--self-test')) selfTest();
  const r = check({
    openapi: fs.readFileSync(opt('--openapi', path.join(root, 'protocol/openapi.yaml')), 'utf8'),
    exemptions: fs.readFileSync(opt('--exemptions', path.join(root, 'protocol/tui-coverage-exemptions.yaml')), 'utf8'),
    cliFiles: loadCli(opt('--cli-dir', path.join(root, 'rust/apps/agent24-cli/src'))),
  });
  if (a.includes('--list-uncovered')) for (const o of r.ops.filter((o) => !r.coveredIds.has(o.id))) console.log(o.id, o.method, o.path);
  else { report(r); process.exit(r.errors.length ? 1 : 0); }
}
