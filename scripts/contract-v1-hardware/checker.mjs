// v1 硬件 schema 兼容 checker：只比较某个端点 GET 200 响应的"旧→新"，
// 依据 SPEC-002 v1 只做加法。不是泛化 OpenAPI diff 工具。纯函数，无 IO。

const refName = (r) => r.replace(/^#\//, '').split('/')
const typesOf = (s) => (s.type === undefined ? [] : [].concat(s.type)).slice().sort().join('|')

function deref(doc, schema, seen = new Set()) {
  let cur = schema
  while (cur && typeof cur.$ref === 'string') {
    if (!cur.$ref.startsWith('#/')) throw new Error(`不支持非本地 $ref: ${cur.$ref}`)
    if (seen.has(cur.$ref)) throw new Error(`$ref 循环: ${cur.$ref}`)
    seen.add(cur.$ref)
    let next = doc
    for (const k of refName(cur.$ref)) next = next?.[k]
    if (next === undefined) throw new Error(`$ref 无法解析: ${cur.$ref}`)
    cur = next
  }
  return cur ?? {}
}

// 取端点 GET 200 application/json 的 schema（顶层 $ref 未展开）。
export function endpointSchema(doc, path, method = 'get') {
  const op = doc?.paths?.[path]?.[method]
  if (!op) throw new Error(`端点不存在: ${method.toUpperCase()} ${path}`)
  const s = op.responses?.['200']?.content?.['application/json']?.schema
  if (!s) throw new Error(`端点无 200 application/json schema: ${method.toUpperCase()} ${path}`)
  return s
}

function walk(oldDoc, newDoc, o, n, at, out, depth) {
  o = deref(oldDoc, o)
  n = deref(newDoc, n)
  if (depth > 32) throw new Error(`schema 嵌套过深: ${at}`)
  const bad = (rule, detail) => out.push({ at, rule, detail })
  if (typesOf(o) !== typesOf(n)) bad('type-changed', `${typesOf(o) || '(无)'} -> ${typesOf(n) || '(无)'}`)
  if (o.enum) {
    const lost = o.enum.filter((v) => !(n.enum ?? o.enum).includes(v))
    if (lost.length) bad('enum-value-removed', JSON.stringify(lost))
  }
  const oProps = o.properties ?? {}
  const nProps = n.properties ?? {}
  const nReq = new Set(n.required ?? [])
  for (const k of o.required ?? []) {
    if (!nReq.has(k)) bad('required-removed', k)
  }
  for (const k of Object.keys(oProps)) {
    if (!(k in nProps)) bad('property-removed', k)
    else walk(oldDoc, newDoc, oProps[k], nProps[k], `${at}.${k}`, out, depth + 1)
  }
  if (o.items) {
    if (!n.items) bad('items-removed', at)
    else walk(oldDoc, newDoc, o.items, n.items, `${at}[]`, out, depth + 1)
  }
}

// 返回违规数组；空数组表示兼容。新增字段、标 deprecated 均可接受。
export function checkHardwareCompat(oldDoc, newDoc, { path = '/decide/profile', method = 'get' } = {}) {
  const out = []
  const root = `${method.toUpperCase()} ${path}`
  walk(oldDoc, newDoc, endpointSchema(oldDoc, path, method), endpointSchema(newDoc, path, method), root, out, 0)
  return out
}
