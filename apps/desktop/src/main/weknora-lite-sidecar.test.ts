import { describe, expect, it } from 'vitest'

import {
  parseWeknoraLiteHandshake,
  parseWeknoraLiteHandshakeLine,
  validateWeknoraLiteEmbedding,
  weknoraLiteChildEnvironment,
  weknoraLiteModelsYaml,
  weknoraLitePolicy,
  type WeknoraLiteLaunchPlan,
} from './weknora-lite-sidecar'

const KEY = 'sk-' + 'z'.repeat(40)
const line = (over: Record<string, unknown> = {}, credential: Record<string, unknown> = {}) => ({
  type: 'ready', protocol: 2, phase: 'process', origin: 'http://127.0.0.1:43123', pid: 4242, version: '0.9.0', commit: 'abc', edition: 'lite',
  credential: { api_key: KEY, knowledge_base_id: 'kb-uuid', expires_at: '2027-11-27T00:00:00Z', ...credential },
  ...over,
})
const embedding = { baseUrl: 'http://127.0.0.1:18300/v1', apiKey: 'idk_' + 'a'.repeat(64), model: 'bge-m3', dimension: 1024 }

describe('parseWeknoraLiteHandshake', () => {
  it('reads origin, key and default KB from protocol 2', () => {
    expect(parseWeknoraLiteHandshake(line(), 4242)).toEqual({
      origin: 'http://127.0.0.1:43123', pid: 4242, version: '0.9.0', apiKey: KEY, knowledgeBaseId: 'kb-uuid', expiresAt: '2027-11-27T00:00:00Z',
    })
  })

  it.each([
    ['protocol 1', line({ protocol: 1 })],
    ['a models line', line({ type: 'models' })],
    ['another pid', line({ pid: 1 })],
    ['a non-loopback origin', line({ origin: 'http://192.168.1.2:43123' })],
    ['localhost by name', line({ origin: 'http://localhost:43123' })],
    ['an origin with a path', line({ origin: 'http://127.0.0.1:43123/x' })],
    ['no credential (old setup_token handshake)', { ...line(), credential: undefined, setup_token: 't' }],
    ['an empty key', line({}, { api_key: '' })],
    ['a key with whitespace', line({}, { api_key: 'a b' })],
    ['no knowledge base', line({}, { knowledge_base_id: '' })],
  ])('refuses %s', (_name, value) => {
    expect(() => parseWeknoraLiteHandshake(value, 4242)).toThrow()
  })

  it('never puts the key in an error', () => {
    try {
      parseWeknoraLiteHandshake(line({ pid: 1 }), 4242)
    } catch (error) {
      expect(String(error)).not.toContain(KEY)
    }
    try {
      parseWeknoraLiteHandshake(line({}, { api_key: `${KEY}\n` }), 4242)
    } catch (error) {
      expect(String(error)).not.toContain(KEY)
    }
  })

  it('PR-Daemon #2108 G5: a broken handshake line never quotes the key back', () => {
    // A syntax error right after the key: V8 would quote the text around it.
    for (const broken of [
      `{"type":"ready","protocol":2,"credential":{"api_key":"${KEY}"x}}`,
      `{"credential":{"api_key":"${KEY}`,
      `{"credential":{"api_key":"${KEY}",}`,
    ]) {
      let message = ''
      try { parseWeknoraLiteHandshakeLine(broken, 4242) } catch (error) { message = String(error) }
      expect(message).toBe('Error: WeKnora Lite handshake is not valid JSON')
    }
  })
})

describe('Lite configuration', () => {
  it('keeps the embedding key out of the YAML file', () => {
    const yaml = weknoraLiteModelsYaml(embedding)
    expect(yaml).not.toContain(embedding.apiKey)
    expect(yaml).toContain('api_key: "${A24LITE_EMBED_KEY}"')
    expect(yaml).toContain('base_url: "http://127.0.0.1:18300/v1"')
    expect(yaml).toContain('dimension: 1024')
  })

  it('allows embeddings to the gateway origin only', () => {
    expect(JSON.parse(weknoraLitePolicy(embedding))).toMatchObject({
      destinations: [{ category: 'embedding', endpoint: 'http://127.0.0.1:18300', model: 'bge-m3' }],
      budget: { max_external_calls: 64, max_input_bytes: 1_048_576, max_inflight_external: 4 },
    })
  })

  it('passes a minimal environment', () => {
    const plan = { modelsConfigPath: '/d/m.yaml', embedding } as unknown as WeknoraLiteLaunchPlan
    const env = weknoraLiteChildEnvironment({ HOME: '/h', PATH: '/bin', A24_TOKEN: 's', DB_PATH: '/x', JWT_SECRET: 'j', REDIS_ADDR: 'r' }, plan)
    expect(Object.keys(env).sort()).toEqual(['A24LITE_EMBED_KEY', 'AGENT24_KNOWLEDGE_POLICY', 'BUILTIN_MODELS_CONFIG', 'HOME', 'PATH', 'SSRF_WHITELIST'])
    expect(env.SSRF_WHITELIST).toBe('127.0.0.1')
    expect(env.A24LITE_EMBED_KEY).toBe(embedding.apiKey)
  })

  it.each([
    { ...embedding, baseUrl: 'https://api.example.com/v1' },
    { ...embedding, baseUrl: 'http://10.0.0.2:18300/v1' },
    { ...embedding, apiKey: '' },
    { ...embedding, model: 'a b' },
    { ...embedding, dimension: 0 },
  ])('refuses an embedding outside the loopback contract (%#)', (bad) => {
    expect(() => validateWeknoraLiteEmbedding(bad)).toThrow()
  })
})
