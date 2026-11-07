import { describe, expect, it } from 'vitest'
import { isPlainHostname } from './knowledge-hostname'

const host = (url: string) => new URL(url).hostname

describe('isPlainHostname (#1643 PR-Daemon C)', () => {
  it('takes DNS names, punycode and IP literals', () => {
    for (const ok of ['https://kb.example.org', 'https://KB.Example.org', 'https://a-b.c1.example', 'https://bücher.example',
      'https://localhost', 'http://127.0.0.1:1', 'http://[::1]:7', 'https://[2001:db8::1]', 'https://10.0.0.2']) {
      expect(isPlainHostname(host(ok)), ok).toBe(true)
    }
  })

  it('refuses what the URL parser keeps but a proxy bypass list would read as a pattern', () => {
    for (const bad of ['https://*', 'https://*.example.org', 'https://a;b.example.org', 'https://a,b.example.org',
      'https://a,*,b.1.2.3.4.sslip.io', 'https://kb_x.example.org', 'https://-kb.example.org', 'https://kb-.example.org',
      'https://a..b', "https://a'b.example", 'https://a!b.example', 'https://a$b.example', 'https://a=b.example']) {
      expect(isPlainHostname(host(bad)), bad).toBe(false)
    }
    expect(isPlainHostname('')).toBe(false)
    expect(isPlainHostname(`${'a'.repeat(63)}.`.repeat(4) + 'org')).toBe(false)
    expect(isPlainHostname('a'.repeat(64))).toBe(false)
  })
})
