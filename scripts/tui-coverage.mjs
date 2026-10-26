#!/usr/bin/env node
// TUI-1 / ADR-037: CLI/TUI must cover every daemon operation in protocol/openapi.yaml,
// or the operation must be listed in protocol/tui-coverage-exemptions.yaml.
//
// "Covered" criterion: a non-test, non-comment `"…/api/v1/<path>"` string literal that sits inside the
// argument list of an HTTP method call (.get/.post/.put/.patch/.delete( — possibly via format!(..) — or the
// tuple holding Method::X) whose method+path template matches an openapi operation ({id}/{name}/{} -> {}).
// Text matching is approximate; the positive/negative controls live in `--self-test`.
//
// Usage: node scripts/tui-coverage.mjs [--openapi F] [--exemptions F] [--cli-dir D] [--json] | --self-test
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const METHODS = ['get', 'post', 'put', 'patch', 'delete'];
// ADR-037 §4: exemption only for purely visual capabilities; everything else is planned work.
const CATEGORIES = ['visual', 'planned-tui-2', 'planned-tui-3'];
const norm = (p) => p.replace(/\{[^}]*\}/g, '{}').replace(/\/+$/, '');

export function parseOpenapi(text) {
  const ops = [];
  let inPaths = false, curPath = null, curMethod = null, needId = null;
  const closeOp = () => { if (needId) throw new Error(`openapi: ${needId} has no operationId (fail-closed)`); };
  text.split('\n').forEach((line, i) => {
    const where = `openapi line ${i + 1}`;
    if (/^paths:/.test(line)) { inPaths = true; return; }
    if (inPaths && /^\S/.test(line)) { closeOp(); needId = null; inPaths = false; }
    if (!inPaths || /^\s*(#.*)?$/.test(line)) return;
    let m;
    if (/^  \S/.test(line)) {
      closeOp(); needId = null; curMethod = null;
      if (!(m = line.match(/^  (["']?)(\/[^"']*)\1:\s*(#.*)?$/))) {
        if (/^  x-/.test(line)) { curPath = null; return; }
        throw new Error(`${where}: unparseable path key (fail-closed): ${line.trim()}`);
      }
      curPath = m[2];
    } else if (/^    \$ref:/.test(line)) {
      throw new Error(`${where}: $ref path item is not supported (fail-closed): ${curPath}`);
    } else if ((m = line.match(/^    (get|post|put|patch|delete):/))) {
      closeOp();
      if (!curPath) throw new Error(`${where}: operation without a path (fail-closed)`);
      curMethod = m[1];
      needId = `${m[1].toUpperCase()} ${curPath}`;
    } else if ((m = line.match(/^      operationId:\s*(\S+)/)) && curPath && curMethod && needId) {
      ops.push({ id: m[1], method: curMethod, path: norm(curPath) });
      needId = null;
    }
  });
  closeOp();
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

// Strip comments (line, trailing, nested block) and #[cfg(test)] tails, string-aware, so
// commented-out or test-only HTTP calls don't count as coverage. Blanked, not removed, to keep offsets.
export function cliCode(src) {
  const cut = src.search(/#\[cfg\(test\)\]/);
  if (cut >= 0) src = src.slice(0, cut);
  let out = '', i = 0, depth = 0;
  while (i < src.length) {
    const c = src[i], n = src[i + 1];
    if (depth > 0) {
      if (c === '/' && n === '*') { depth++; i += 2; } else if (c === '*' && n === '/') { depth--; i += 2; } else i++;
      out += ' ';
    } else if (c === '/' && n === '*') { depth = 1; i += 2; out += ' '; }
    else if (c === '/' && n === '/') { while (i < src.length && src[i] !== '\n') i++; }
    else if (c === '"') { const e = strEnd(src, i); out += src.slice(i, e); i = e; }
    else if (c === "'" && src[i + 2] === "'") { out += src.slice(i, i + 3); i += 3; }
    else if (c === "'" && n === '\\' && src[i + 3] === "'") { out += src.slice(i, i + 4); i += 4; }
    else { out += c; i++; }
  }
  return out;
}

function strEnd(src, i) { // index just past the string literal starting at src[i] === '"'
  for (i++; i < src.length; i++) { if (src[i] === '\\') i++; else if (src[i] === '"') return i + 1; }
  return src.length;
}

function matchClose(code, open) { // index of the ')' matching code[open] === '('
  let d = 0;
  for (let i = open; i < code.length; i++) {
    if (code[i] === '"') { i = strEnd(code, i) - 1; continue; }
    if (code[i] === '(') d++; else if (code[i] === ')' && --d === 0) return i;
  }
  return -1;
}

// A path literal counts only if it sits inside the argument list of the method call:
// `.get( ... )` (possibly wrapped in format!(..)) or the enclosing tuple/call of `Method::X`.
export function cliCalls(files) {
  const calls = new Set();
  const lit = /"((?:[^"\\]|\\.)*)"/g;
  for (const code of files.map(cliCode)) {
    const re = /\.(get|post|put|patch|delete)\s*\(|Method::(GET|POST|PUT|PATCH|DELETE)\b/g;
    let m;
    while ((m = re.exec(code))) {
      let open = m[1] ? m.index + m[0].length - 1 : -1;
      if (!m[1]) { // innermost unclosed '(' before Method::X
        let d = 0;
        for (let i = m.index - 1; i >= 0; i--) {
          if (code[i] === ')') d++; else if (code[i] === '(') { if (d === 0) { open = i; break; } d--; }
        }
      }
      const close = open < 0 ? -1 : matchClose(code, open);
      if (close < 0) continue;
      const args = code.slice(open, close);
      for (const l of args.matchAll(lit)) {
        const pm = l[1].match(/\/api\/v1(\/[^"?\s]*)/);
        if (pm) calls.add(`${(m[1] || m[2]).toLowerCase()} ${norm(pm[1])}`);
      }
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
  // --- Codex challenge round 1 (counter-examples; each must be red before the fix, green after) ---
  const withCli = (extra) => check({ openapi, exemptions, cliFiles: [...cliFiles, extra] });
  // Probe an exempted GET the real CLI does not call. listTimings was the probe
  // until TUI-2a2 covered it; a covered probe would make the negative controls
  // pass for the wrong reason. Assert on coverage, not on errors.
  const stillExempt = (r) => !r.coveredIds.has('sin90ListDirections');
  const probe = '/api/v1/sin90/directions';
  expect('block-commented request is NOT coverage', stillExempt(withCli(`/* client().get(format!("{}${probe}", b)) */`)));
  expect('path literal outside the call args is NOT coverage', stillExempt(withCli(`fn x(){ client().get("/old"); let s = "${probe}"; }`)));
  expect('trailing-comment request is NOT coverage', stillExempt(withCli(`fn x(){ foo(); } // client().get("${probe}")`)));
  expect('real .get(format!(..)) call IS coverage', !stillExempt(withCli(`fn x(){ bearer(&ep, client().get(format!("{}${probe}", ep.base))); }`)));
  expect('Method::GET tuple call IS coverage', !stillExempt(withCli(`fn x(){ let t = (reqwest::Method::GET, "${probe}".to_owned(), None); }`)));
  const withPath = (key, extra = '      operationId: quotedNew\n') =>
    openapi.replace(/^  \/chat:/m, `  ${key}:\n    get:\n${extra}      responses: {}\n  /chat:`);
  for (const key of ['"/new"', "'/new'"]) {
    const r = check({ openapi: withPath(key), exemptions, cliFiles });
    expect(`quoted path key ${key} parsed as a new uncovered op`, r.total === base.total + 1 && r.errors.some((e) => e.includes('uncovered: quotedNew')));
  }
  const throws = (fn) => { try { fn(); return false; } catch { return true; } };
  expect('$ref path item fails closed', throws(() => check({ openapi: openapi.replace(/^  \/health:/m, "  /refd:\n    $ref: './x.yaml'\n  /health:"), exemptions, cliFiles })));
  expect('unparseable path-level line fails closed', throws(() => check({ openapi: openapi.replace(/^  \/health:/m, "  ?weird key:\n    get: {}\n  /health:"), exemptions, cliFiles })));
  expect('method without operationId fails closed', throws(() => check({ openapi: withPath('/noid', ''), exemptions, cliFiles })));
  const f5 = check({ openapi, exemptions: exemptions + '\n  - operationId: getUsage\n    category: desktop-internal\n    fallback: x\n', cliFiles });
  expect('category desktop-internal is illegal (ADR-037 §4)', f5.errors.some((e) => e.includes('category')));
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
