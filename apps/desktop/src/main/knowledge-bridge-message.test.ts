import { describe, expect, it } from 'vitest'
import {
  BRIDGE_MAX_HOST_MESSAGE_BYTES,
  encodeHostMessage,
  envelopeId,
  hasForbiddenKey,
  isForbiddenKey,
  parseBridgeJson,
  readPageMessage,
  versionOf,
  type Json,
} from './knowledge-bridge-message'

const b = (text: string): Uint8Array => Buffer.from(text, 'utf8')
const parse = (text: string) => parseBridgeJson(b(text))
const ID = '01J9ZQ3V6R8XKQ4M2N5P7T9W0A'
const REF = { knowledge_base_id: 'kb-1', knowledge_id: 'doc-1', chunk_id: 'c-1', source_revision: 'r1', index_build_id: 'b1', chunk_revision: 3, content_tag: 'tag-1' }
const env = (type: string, payload: unknown, extra: Record<string, unknown> = {}) =>
  ({ v: 2, direction: 'page_to_host', type, id: ID, generation: 1, payload, ...extra })
const read = (value: unknown) => readPageMessage(JSON.parse(JSON.stringify(value)) as Json)

describe('parseBridgeJson (U6.4 ⑧)', () => {
  it('reads plain JSON, and __proto__ is just a key', () => {
    expect(parse('{"a":[1,true,null,"x"],"b":{"c":-1.5e2}}')).toEqual({ a: [1, true, null, 'x'], b: { c: -150 } })
    const o = parse('{"__proto__":{"x":1}}') as Record<string, unknown>
    expect(Object.keys(o)).toEqual(['__proto__'])
    expect(({} as Record<string, unknown>).x).toBeUndefined()
  })

  it.each([
    ['duplicate key', '{"a":1,"a":2}'],
    ['nested duplicate key', '{"a":{"b":1,"b":1}}'],
    ['five levels', '{"a":{"b":{"c":{"d":{}}}}}'],
    ['five levels via arrays', '{"a":[[[[]]]]}'],
    ['33 items', `{"a":[${Array(33).fill(0).join(',')}]}`],
    ['string over 2 KiB', `{"a":"${'é'.repeat(1025)}"}`],
    ['key over 2 KiB', `{"${'k'.repeat(2049)}":1}`],
    ['trailing data', '{"a":1} x'],
    ['two values', '{}{}'],
    ['lone surrogate', '{"a":"\\ud800"}'],
    ['raw control character', '{"a":"\u0001"}'],
    ['bad escape', '{"a":"\\x"}'],
    ['leading zero', '{"a":01}'],
    ['NaN', '{"a":NaN}'],
    ['unquoted key', '{a:1}'],
    ['trailing comma', '{"a":1,}'],
    ['BOM', '﻿{}'],
    ['empty', ''],
  ])('refuses %s', (_name, text) => {
    expect(parse(text)).toBeUndefined()
  })

  it('takes the limits themselves', () => {
    expect(parse('{"a":{"b":{"c":{}}}}')).toBeDefined()
    expect(parse(`{"a":[${Array(32).fill(0).join(',')}]}`)).toBeDefined()
    expect(parse(`{"a":"${'x'.repeat(2048)}"}`)).toBeDefined()
    expect(parse('{"a":"\\ud83d\\ude00"}')).toEqual({ a: '😀' })
  })

  it('refuses bytes that are not UTF-8', () => {
    expect(parseBridgeJson(Uint8Array.from([0x7b, 0x22, 0x61, 0x22, 0x3a, 0x22, 0xff, 0x22, 0x7d]))).toBeUndefined()
  })
})

describe('forbidden keys (U7.4)', () => {
  it('matches after lower-casing and dropping _ and -, never by substring', () => {
    for (const k of ['url', 'URL', 'Web_Contents', 'web-contents', 'Authorization', '__proto__', 'prototype', 'ORIGIN', 's_e-n_d_e_r'])
      expect(isForbiddenKey(k)).toBe(true)
    for (const k of ['origins', 'urls', 'paths2', 'myurl', 'tokenizer', 'state'])
      expect(isForbiddenKey(k)).toBe(false)
  })

  it('looks at every depth, arrays included', () => {
    expect(hasForbiddenKey(parse('{"a":[{"b":{"Origin":1}}]}')!)).toBe(true)
    expect(hasForbiddenKey(parse('{"a":[{"b":{"origin_name":1}}]}')!)).toBe(false)
    expect(hasForbiddenKey(parse('{"__proto__":1}')!)).toBe(true)
    // A value that spells a forbidden key is not a key.
    expect(hasForbiddenKey(parse('{"a":"url"}')!)).toBe(false)
  })
})

describe('version and envelope id', () => {
  it('⑪: 2 is supported, anything else is not, missing falls to ⑫', () => {
    expect(versionOf({ v: 2 })).toBe('supported')
    expect(versionOf({ v: 3 })).toBe('unsupported')
    expect(versionOf({ v: '2' })).toBe('unsupported')
    expect(versionOf({})).toBe('missing')
    expect(versionOf([])).toBe('missing')
  })

  it('gives the id only when it is a ULID', () => {
    expect(envelopeId({ id: ID })).toBe(ID)
    expect(envelopeId({ id: ID.toLowerCase() })).toBeUndefined()
    expect(envelopeId({ id: '01J9ZQ3V6R8XKQ4M2N5P7T9W0I' })).toBeUndefined()
    expect(envelopeId('x')).toBeUndefined()
  })
})

describe('readPageMessage (U6.4 ⑫, U7.1, U7.2)', () => {
  it('reads the four page messages', () => {
    expect(read(env('status', { state: 'ready' }))).toEqual({ ok: true, message: { id: ID, generation: 1, payload: { type: 'status', state: 'ready' }, helloRequest: false } })
    expect(read(env('status', { state: 'error', error_code: 'unauthorized' }))).toMatchObject({ ok: true })
    expect(read(env('status', { state: 'error' }))).toMatchObject({ ok: true })
    expect(read(env('citation', REF))).toEqual({ ok: true, message: { id: ID, generation: 1, payload: { type: 'citation', citation: REF }, helloRequest: false } })
    expect(read(env('cancel', { target_id: ID }))).toMatchObject({ ok: true, message: { payload: { type: 'cancel', target_id: ID } } })
    expect(read(env('return_to_chat_intent', { citations: [REF, REF] }))).toMatchObject({ ok: true, message: { payload: { citations: [REF, REF] } } })
    expect(read(env('return_to_chat_intent', { citations: [] }))).toMatchObject({ ok: true })
  })

  it('a generation-0 status is a hello request; no other type may carry 0', () => {
    expect(read(env('status', { state: 'loading' }, { generation: 0 }))).toMatchObject({ ok: true, message: { helloRequest: true } })
    expect(read(env('citation', REF, { generation: 0 }))).toEqual({ ok: false, counted: true })
  })

  it.each([
    ['extra envelope field', env('status', { state: 'ready' }, { extra: 1 })],
    ['in_reply_to from the page', env('status', { state: 'ready' }, { in_reply_to: ID })],
    ['missing v', (() => { const e: Record<string, unknown> = env('status', { state: 'ready' }); delete e.v; return e })()],
    ['host direction', env('status', { state: 'ready' }, { direction: 'host_to_page' })],
    ['host type', env('reply', { result: 'done' })],
    ['unknown type', env('search', {})],
    ['bad id', env('status', { state: 'ready' }, { id: 'x' })],
    ['negative generation', env('status', { state: 'ready' }, { generation: -1 })],
    ['fractional generation', env('status', { state: 'ready' }, { generation: 1.5 })],
    ['generation past 2^53-1', env('status', { state: 'ready' }, { generation: 2 ** 53 })],
    ['status state', env('status', { state: 'done' })],
    ['error_code without error', env('status', { state: 'ready', error_code: 'internal' })],
    ['unknown error_code', env('status', { state: 'error', error_code: 'boom' })],
    ['extra status field', env('status', { state: 'ready', note: 'x' })],
    ['citation missing a field', env('citation', { ...REF, content_tag: undefined })],
    ['citation extra field', env('citation', { ...REF, title: 't' })],
    ['chunk_revision past 2^31-1', env('citation', { ...REF, chunk_revision: 2 ** 31 })],
    ['chunk_revision fractional', env('citation', { ...REF, chunk_revision: 1.5 })],
    ['empty ID', env('citation', { ...REF, chunk_id: '' })],
    ['ID over 128 bytes', env('citation', { ...REF, chunk_id: 'é'.repeat(65) })],
    ['control character in an ID', env('citation', { ...REF, chunk_id: 'a\u0007' })],
    ['cancel target not a ULID', env('cancel', { target_id: 'x' })],
    ['nine citations', env('return_to_chat_intent', { citations: Array(9).fill(REF) })],
    ['a bad citation in an intent', env('return_to_chat_intent', { citations: [REF, { ...REF, chunk_revision: -1 }] })],
    ['payload not an object', env('status', 'ready')],
  ])('counts %s as a schema violation', (_name, value) => {
    expect(read(value)).toEqual({ ok: false, counted: true })
  })

  it.each(['a/b', 'a\\b', 'a:b', 'a..b', 'a b', 'a\u00a0b'])('refuses the ID %j by default without counting it (decision #21)', (id) => {
    expect(read(env('citation', { ...REF, knowledge_id: id }))).toEqual({ ok: false, counted: false })
    expect(read(env('return_to_chat_intent', { citations: [REF, { ...REF, chunk_id: id }] }))).toEqual({ ok: false, counted: false })
  })

  it('a bad ID next to a refused one is counted', () => {
    expect(read(env('return_to_chat_intent', { citations: [{ ...REF, chunk_id: 'a/b' }, { ...REF, chunk_id: '' }] }))).toEqual({ ok: false, counted: true })
  })
})

describe('encodeHostMessage (U7.1, U7.3)', () => {
  it('builds the envelope; in_reply_to only on replies', () => {
    const hello = JSON.parse(encodeHostMessage({ type: 'hello', payload: { supported: [2], view_instance_id: 'A'.repeat(22) } }, ID, 7)!)
    expect(hello).toEqual({ v: 2, direction: 'host_to_page', type: 'hello', id: ID, generation: 7, payload: { supported: [2], view_instance_id: 'A'.repeat(22) } })
    const reply = JSON.parse(encodeHostMessage({ type: 'reply', in_reply_to: ID, payload: { result: 'done' } }, ID, 1)!)
    expect(reply.in_reply_to).toBe(ID)
    expect(Object.keys(reply)).toEqual(['v', 'direction', 'type', 'id', 'generation', 'in_reply_to', 'payload'])
  })

  it('does not send a forbidden key or more than 32 KiB', () => {
    expect(encodeHostMessage({ type: 'state', payload: { profile_state: 'ready', theme: 'dark', locale: 'en', ...({ url: 'x' } as object) } as never }, ID, 1)).toBeNull()
    const big = 'x'.repeat(BRIDGE_MAX_HOST_MESSAGE_BYTES)
    expect(encodeHostMessage({ type: 'state', payload: { profile_state: big, theme: 'dark', locale: 'en' } }, ID, 1)).toBeNull()
  })
})
