import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawnSync } from 'node:child_process'
import { checkHardwareCompat, endpointSchema } from './checker.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const P = '/decide/profile'
// 两层 $ref：端点 -> Wrapper -> Hw
const mk = (hw, extra = {}) => ({
  paths: {
    [P]: { get: { responses: { 200: { content: { 'application/json': { schema: { $ref: '#/components/schemas/Wrapper' } } } } } } },
    ...extra,
  },
  components: { schemas: {
    Wrapper: { type: 'object', required: ['profile'], properties: { profile: { $ref: '#/components/schemas/Hw' } } },
    Hw: hw,
  } },
})
const base = () => ({
  type: 'object',
  required: ['mem', 'disk'],
  properties: { mem: { type: 'integer' }, disk: { type: 'integer' }, mode: { type: 'string', enum: ['a', 'b'] }, gpus: { type: 'array', items: { type: 'object', required: ['name'], properties: { name: { type: 'string' } } } } },
})
const rules = (o, n, opts) => checkHardwareCompat(o, n, opts).map((v) => v.rule).sort()

test('旧 required 被删必须拒绝（含属性一起删）', () => {
  const n = base(); n.required = ['mem']; delete n.properties.disk
  assert.deepEqual(rules(mk(base()), mk(n)), ['property-removed', 'required-removed'])
})
test('仅从 required 移除也拒绝', () => {
  const n = base(); n.required = ['mem']
  assert.deepEqual(rules(mk(base()), mk(n)), ['required-removed'])
})
test('类型改坏必须拒绝（含 nullable 变化）', () => {
  const n = base(); n.properties.mem = { type: 'string' }
  assert.deepEqual(rules(mk(base()), mk(n)), ['type-changed'])
  const m = base(); m.properties.mem = { type: ['integer', 'null'] }
  assert.deepEqual(rules(mk(base()), mk(m)), ['type-changed'])
})
test('enum 值被删拒绝，新增值可接受', () => {
  const n = base(); n.properties.mode.enum = ['a']
  assert.deepEqual(rules(mk(base()), mk(n)), ['enum-value-removed'])
  const m = base(); m.properties.mode.enum = ['a', 'b', 'c']
  assert.deepEqual(rules(mk(base()), mk(m)), [])
})
test('嵌套数组 items 里 required 被删拒绝', () => {
  const n = base(); n.properties.gpus.items.required = []
  assert.deepEqual(rules(mk(base()), mk(n)), ['required-removed'])
})
test('新增字段（含 required）与标 deprecated 可接受', () => {
  const n = base()
  n.required.push('extra')
  n.properties.extra = { type: 'boolean' }
  n.properties.disk = { type: 'integer', deprecated: true, description: 'replaced' }
  assert.deepEqual(rules(mk(base()), mk(n)), [])
})
test('端点 $ref 展开：旧新 ref 指向不同名 schema 也能比较', () => {
  const n = mk(base())
  n.components.schemas.Hw2 = { ...base(), required: ['mem'] }
  n.components.schemas.Wrapper.properties.profile = { $ref: '#/components/schemas/Hw2' }
  assert.deepEqual(rules(mk(base()), n), ['required-removed'])
})
test('误匹配反例：其它端点的同名 schema 变化不影响', () => {
  const other = { '/other': { get: { responses: { 200: { content: { 'application/json': { schema: { $ref: '#/components/schemas/Hw' } } } } } } } }
  const n = base(); n.required = ['mem']
  const o = mk(base(), other)
  const newDoc = mk(base(), other)
  newDoc.paths['/other'].get.responses[200].content['application/json'].schema = n
  assert.deepEqual(rules(o, newDoc), [])
})
test('误匹配反例：端点缺失/无 200 schema/坏 ref 报错而非静默通过', () => {
  assert.throws(() => checkHardwareCompat(mk(base()), mk(base()), { path: '/nope' }), /端点不存在/)
  assert.throws(() => checkHardwareCompat(mk(base()), { paths: { [P]: { get: { responses: {} } } } }), /无 200/)
  const bad = mk(base()); bad.components.schemas.Wrapper.properties.profile = { $ref: '#/components/schemas/Missing' }
  assert.throws(() => checkHardwareCompat(mk(base()), bad), /无法解析/)
  assert.throws(() => endpointSchema({}, P), /端点不存在/)
})
test('$ref 循环报错', () => {
  const d = mk(base()); d.components.schemas.Hw = { $ref: '#/components/schemas/Hw2' }
  d.components.schemas.Hw2 = { $ref: '#/components/schemas/Hw' }
  assert.throws(() => checkHardwareCompat(d, d), /循环/)
})
test('结构性递归 schema 触发深度保护而非栈溢出', () => {
  const d = mk(base()); d.components.schemas.Hw = { type: 'object', properties: { p: { $ref: '#/components/schemas/Hw' } } }
  assert.throws(() => checkHardwareCompat(d, d), /嵌套过深/)
})

// ---- CLI 与真实历史（只读公开 Git 历史；子进程 env allowlist + fake HOME） ----
const OLD = 'git:6072fd373142eb3dbf1a6a4a3222e6eec82c74f6:protocol/openapi.yaml'
const tmp = mkdtempSync(join(tmpdir(), 'v1hw-'))
const cli = (...a) => spawnSync('node', [join(here, 'cli.mjs'), ...a], {
  cwd: join(here, '..', '..'), encoding: 'utf8',
  env: { PATH: process.env.PATH, HOME: tmp, XDG_CONFIG_HOME: join(tmp, '.config'), TMPDIR: tmp },
})
test.after(() => rmSync(tmp, { recursive: true, force: true }))

test('CLI：真实 6072 → 当前 openapi 预期 RED（free_disk_bytes）', (t) => {
  const r = cli('--old', OLD, '--new', 'protocol/openapi.yaml')
  if (r.status === 2) return t.skip(`历史对象不可用: ${r.stderr.trim()}`)
  assert.equal(r.status, 1, r.stdout + r.stderr)
  assert.match(r.stdout, /required-removed .*free_disk_bytes/)
})
test('CLI 变异反证：把 free_disk_bytes 加回当前 openapi 副本 → GREEN', (t) => {
  let text = readFileSync(join(here, '..', '..', 'protocol', 'openapi.yaml'), 'utf8')
  const req = 'required: [total_mem_bytes, avail_mem_bytes, arch, cpu_cores, accelerator, os]'
  assert.ok(text.includes(req), '预期的 required 行不存在，fixture 漂移')
  text = text.replace(req, 'required: [total_mem_bytes, avail_mem_bytes, arch, cpu_cores, accelerator, free_disk_bytes, os]')
    .replace('        os:\n          type: string\n          examples: [macos', '        free_disk_bytes:\n          type: integer\n          deprecated: true\n        os:\n          type: string\n          examples: [macos')
  const f = join(tmp, 'restored.yaml'); writeFileSync(f, text)
  const r = cli('--old', OLD, '--new', f)
  if (r.status === 2) return t.skip(`历史对象不可用: ${r.stderr.trim()}`)
  assert.equal(r.status, 0, r.stdout + r.stderr)
})
test('CLI 用法错误 exit 2', () => assert.equal(cli().status, 2))
