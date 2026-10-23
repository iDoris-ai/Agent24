#!/usr/bin/env node
// 用法: node cli.mjs --old <file|git:REV:path> --new <file|git:REV:path> [--path /decide/profile]
// 退出码: 0 兼容, 1 有违规, 2 用法/读取错误。
import { readFileSync } from 'node:fs'
import { execFileSync } from 'node:child_process'
import { createRequire } from 'node:module'
import { checkHardwareCompat } from './checker.mjs'

const require = createRequire(new URL('../../packages/contract-tests/package.json', import.meta.url))
const { parse } = require('yaml')

const load = (src) => {
  const m = /^git:([0-9a-f]{7,40}):(.+)$/.exec(src)
  const text = m
    ? execFileSync('git', ['show', `${m[1]}:${m[2]}`], { encoding: 'utf8', maxBuffer: 64 << 20 })
    : readFileSync(src, 'utf8')
  return parse(text)
}

const args = Object.fromEntries(process.argv.slice(2).reduce((a, v, i, all) => (v.startsWith('--') ? [...a, [v.slice(2), all[i + 1]]] : a), []))
if (!args.old || !args.new) {
  console.error('用法: cli.mjs --old <file|git:REV:path> --new <file|git:REV:path> [--path /decide/profile]')
  process.exit(2)
}
try {
  const violations = checkHardwareCompat(load(args.old), load(args.new), args.path ? { path: args.path } : {})
  for (const v of violations) console.log(`VIOLATION ${v.rule} ${v.at} ${v.detail}`)
  console.log(`violations=${violations.length}`)
  process.exit(violations.length ? 1 : 0)
} catch (e) {
  console.error(`ERROR ${e.message}`)
  process.exit(2)
}
