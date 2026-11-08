import { describe, expect, it } from 'vitest'
import {
  MAX_REDACT_INPUT,
  placeholder,
  postScan,
  preRedact,
  type RedactionCategory,
  type RedactionHit,
} from './knowledge-entry-redact'

const JWT = 'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c'
const VALID_IDS = ['110101199003078873', '11010519491231002X']
const DUAL_ID = '110101199001010250'
const VISA = '4111111111111111'
const MC = '5500000000000004'

function expectShape(hits: RedactionHit[]): void {
  for (const hit of hits) {
    expect(Object.keys(hit).sort()).toEqual(['category', 'end', 'start'])
    expect(hit.end).toBeGreaterThan(hit.start)
  }
  for (let i = 1; i < hits.length; i += 1) {
    expect(hits[i].start).toBeGreaterThanOrEqual(hits[i - 1].end)
  }
}

function categories(text: string): RedactionCategory[] {
  const hits = postScan(text)
  expectShape(hits)
  return hits.map(hit => hit.category)
}

function redact(text: string): string {
  const out = preRedact(text)
  expect(postScan(out.text)).toEqual([])
  return out.text
}

describe('placeholder', () => {
  it('uses the fixed label for each category', () => {
    expect(placeholder('private_key')).toBe('〔已脱敏·私钥〕')
    expect(placeholder('credential')).toBe('〔已脱敏·凭据〕')
    expect(placeholder('email')).toBe('〔已脱敏·邮箱〕')
    expect(placeholder('phone')).toBe('〔已脱敏·电话〕')
    expect(placeholder('id_card')).toBe('〔已脱敏·证件号〕')
    expect(placeholder('bank_card')).toBe('〔已脱敏·银行卡号〕')
    expect(placeholder('path')).toBe('〔已脱敏·路径〕')
    expect(placeholder('internal_address')).toBe('〔已脱敏·内网地址〕')
  })

  it('is not a hit on its own or after preRedact', () => {
    const raw = '前文〔已脱敏·邮箱〕后文〔已脱敏·凭据〕〔已脱敏·私钥〕'
    expect(postScan(raw)).toEqual([])
    expect(preRedact(raw)).toEqual({ text: raw, counts: {} })
    expect(postScan(preRedact(`password=${placeholder('credential')}`).text)).toEqual([])
    expect(redact('ada@example.com 13800138000')).not.toMatch(/ada@|13800138000/)
  })
})

describe('private_key', () => {
  it('matches a full RSA block', () => {
    const block = [
      '-----BEGIN RSA PRIVATE KEY-----',
      'MIIBogIBAAJB',
      '-----END RSA PRIVATE KEY-----',
    ].join('\n')
    expect(redact(`before\n${block}\nafter`)).toBe('before\n〔已脱敏·私钥〕\nafter')
    expect(categories(block)).toEqual(['private_key'])
  })

  it('matches a full OPENSSH block', () => {
    const block = [
      '-----BEGIN OPENSSH PRIVATE KEY-----',
      'AAAA',
      '-----END OPENSSH PRIVATE KEY-----',
    ].join('\n')
    expect(redact(block)).toBe('〔已脱敏·私钥〕')
  })

  it('without END matches only the BEGIN line', () => {
    const text = '-----BEGIN EC PRIVATE KEY-----\nkeep this line\nstill here\n'
    const hits = postScan(text)
    expectShape(hits)
    expect(hits).toHaveLength(1)
    expect(text.slice(hits[0].start, hits[0].end)).toBe('-----BEGIN EC PRIVATE KEY-----')
    expect(redact(text)).toBe('〔已脱敏·私钥〕\nkeep this line\nstill here\n')
  })

  it('does not match the lines that follow a BEGIN with no END', () => {
    const text = '-----BEGIN PRIVATE KEY-----\nnot a key body\n'
    const hits = postScan(text)
    expect(hits).toHaveLength(1)
    expect(text.slice(hits[0].end)).toBe('\nnot a key body\n')
    expect(categories('-----BEGIN PUBLIC KEY-----\nAAA\n-----END PUBLIC KEY-----')).toEqual([])
  })
})

describe('credential', () => {
  it('matches sk-, github, aws, slack, jwt, and bearer tokens', () => {
    const samples = [
      `sk-${'a'.repeat(16)}`,
      `sk-${'A1_-'.repeat(8)}`,
      `ghp_${'a'.repeat(20)}`,
      `gho_${'b'.repeat(20)}`,
      `ghs_${'c'.repeat(20)}`,
      `ghu_${'d'.repeat(20)}`,
      `github_pat_${'e'.repeat(20)}`,
      `AKIA${'A'.repeat(16)}`,
      `xoxb-${'a'.repeat(10)}`,
      `xoxp-${'1'.repeat(12)}`,
      JWT,
      `Bearer ${'t'.repeat(16)}`,
    ]
    for (const sample of samples) {
      expect(redact(`use ${sample} now`), sample).toBe('use 〔已脱敏·凭据〕 now')
    }
  })

  it('keeps the key and replaces only the assigned value', () => {
    expect(redact('password=s3cret')).toBe('password=〔已脱敏·凭据〕')
    expect(redact('api_key: abcdefghijklmnop')).toBe('api_key: 〔已脱敏·凭据〕')
    expect(redact('token = hunter2')).toBe('token = 〔已脱敏·凭据〕')
    expect(redact('passwd:s3cret')).toBe('passwd:〔已脱敏·凭据〕')
    expect(redact('access_key=s3cret')).toBe('access_key=〔已脱敏·凭据〕')
  })

  it('matches keys and Bearer in any ASCII case', () => {
    expect(redact('PASSWORD=hunter2')).toBe('PASSWORD=〔已脱敏·凭据〕')
    expect(redact('Password: hunter2')).toBe('Password: 〔已脱敏·凭据〕')
    expect(redact('API_KEY=abc')).toBe('API_KEY=〔已脱敏·凭据〕')
    expect(redact('Secret = x1')).toBe('Secret = 〔已脱敏·凭据〕')
    expect(redact('authorization: bearer abcdefghijklmnopqrstuvwx')).toBe('authorization: 〔已脱敏·凭据〕')
    expect(redact('BEARER abcdefghijklmnopqrstuvwx')).toBe('〔已脱敏·凭据〕')
    // Offsets are those of the original text, whatever its case.
    const text = 'İ x PASSWORD=hunter2'
    const [hit] = postScan(text)
    expect(text.slice(hit.start, hit.end)).toBe('hunter2')
  })

  it('does not match a bare token or password word', () => {
    expect(categories('the token and the password stay')).toEqual([])
    expect(redact('please reset the password later')).toBe('please reset the password later')
  })

  it('does not match sk- shorter than 16 characters', () => {
    expect(categories('sk-1234567890abcde')).toEqual([])
    expect(categories('sk-short')).toEqual([])
  })
})

describe('email', () => {
  it('matches a plain address and a tagged one', () => {
    expect(redact('write ada@example.com today')).toBe('write 〔已脱敏·邮箱〕 today')
    expect(redact('to first.last+tag@sub.example.co.uk.')).toBe('to 〔已脱敏·邮箱〕.')
  })

  it('does not match text without @', () => {
    expect(categories('ada.example.com')).toEqual([])
    expect(categories('just words')).toEqual([])
  })
})

describe('phone', () => {
  it('matches mainland mobiles with separators and an 86 prefix', () => {
    expect(redact('call 13800138000 now')).toBe('call 〔已脱敏·电话〕 now')
    expect(redact('call +86 139-1234-5678 now')).toBe('call 〔已脱敏·电话〕 now')
    expect(redact('call 138 0013 8000 now')).toBe('call 〔已脱敏·电话〕 now')
    expect(redact('call 8613912345678 now')).toBe('call 〔已脱敏·电话〕 now')
  })

  it('matches international numbers with and without one separator', () => {
    expect(redact('call +1 2025550199 now')).toBe('call 〔已脱敏·电话〕 now')
    expect(redact('call +442071838750 now')).toBe('call 〔已脱敏·电话〕 now')
  })

  it('does not match a segment of 12 or more consecutive digits', () => {
    expect(categories('138001380001')).toEqual([])
    expect(categories('991380013800099')).toEqual([])
  })

  it('does not match a version number', () => {
    expect(categories('version 1.3.4')).toEqual([])
    expect(categories('build 1.3.4-rc1')).toEqual([])
  })
})

describe('id_card', () => {
  it('matches a valid checksum, including one that ends in X', () => {
    for (const id of VALID_IDS) {
      expect(redact(`号码 ${id} 结束`), id).toBe('号码 〔已脱敏·证件号〕 结束')
    }
  })

  it('drops the hit when one digit of a valid number is changed', () => {
    expect(categories('110101199003078874')).toEqual([])
    expect(categories('110105194912310020')).toEqual([])
  })

  it('does not match when a letter is glued to the number', () => {
    expect(categories(`A${VALID_IDS[0]}`)).toEqual([])
    expect(categories(`${VALID_IDS[0]}B`)).toEqual([])
  })
})

describe('bank_card', () => {
  it('matches a Luhn-valid number, including groups of four', () => {
    expect(redact(`卡 ${VISA} 结束`)).toBe('卡 〔已脱敏·银行卡号〕 结束')
    expect(redact(`卡 ${MC} 结束`)).toBe('卡 〔已脱敏·银行卡号〕 结束')
    expect(redact('卡 4111-1111-1111-1111 结束')).toBe('卡 〔已脱敏·银行卡号〕 结束')
    expect(redact('卡 4111 1111 1111 1111 结束')).toBe('卡 〔已脱敏·银行卡号〕 结束')
  })

  it('does not match a long number that fails Luhn', () => {
    expect(categories('4111111111111112')).toEqual([])
    expect(categories('5500000000000005')).toEqual([])
  })

  it('does not classify a span already matched as an id card', () => {
    const hits = postScan(`证件 ${DUAL_ID} 在此`)
    expectShape(hits)
    expect(hits.map(hit => hit.category)).toEqual(['id_card'])
    expect(redact(`证件 ${DUAL_ID} 在此`)).toBe('证件 〔已脱敏·证件号〕 在此')
  })
})

describe('path', () => {
  it('matches home, tilde, windows, and UNC paths', () => {
    expect(redact('see /Users/ada/notes.txt please')).toBe('see 〔已脱敏·路径〕 please')
    expect(redact('see ~/secrets/key please')).toBe('see 〔已脱敏·路径〕 please')
    expect(redact('see C:\\Users\\ada\\a.txt please')).toBe('see 〔已脱敏·路径〕 please')
    expect(redact('see \\\\server\\share\\a.txt please')).toBe('see 〔已脱敏·路径〕 please')
    expect(redact('配置在 /etc/hosts。')).toBe('配置在 〔已脱敏·路径〕。')
    expect(redact('盘 /Volumes/UltraDisk/a 上')).toBe('盘 〔已脱敏·路径〕 上')
  })

  it('stops at a quote and does not take the URL path', () => {
    expect(redact('"/Users/ada/a"')).toBe('"〔已脱敏·路径〕"')
    expect(categories('https://a.com/Users/x')).toEqual([])
    expect(categories('https://cdn.example.com/home/ada/x')).toEqual([])
    expect(redact('see https://a.com/Users/x and /Users/me')).toBe('see https://a.com/Users/x and 〔已脱敏·路径〕')
  })
})

describe('internal_address', () => {
  it('matches private IPv4, localhost with a port, and internal hostnames', () => {
    expect(redact('go 10.1.2.3 now')).toBe('go 〔已脱敏·内网地址〕 now')
    expect(redact('go 192.168.0.10:8080 now')).toBe('go 〔已脱敏·内网地址〕 now')
    expect(redact('go 172.16.5.4 now')).toBe('go 〔已脱敏·内网地址〕 now')
    expect(redact('go 172.31.255.255 now')).toBe('go 〔已脱敏·内网地址〕 now')
    expect(redact('go 127.0.0.1 now')).toBe('go 〔已脱敏·内网地址〕 now')
    expect(redact('go localhost:3000 now')).toBe('go 〔已脱敏·内网地址〕 now')
    expect(redact('go nas.local now')).toBe('go 〔已脱敏·内网地址〕 now')
    expect(redact('go db.corp now')).toBe('go 〔已脱敏·内网地址〕 now')
    expect(redact('go api.internal now')).toBe('go 〔已脱敏·内网地址〕 now')
  })

  it('does not match a public IP', () => {
    expect(categories('dns 8.8.8.8')).toEqual([])
    expect(categories('also 1.1.1.1')).toEqual([])
  })

  it('does not match a short version, a bare localhost, or an out-of-range prefix', () => {
    expect(categories('version 10.2')).toEqual([])
    expect(categories('version 10.2.3')).toEqual([])
    expect(categories('host localhost')).toEqual([])
    expect(categories('go 172.15.0.1 now')).toEqual([])
    expect(categories('go 172.32.0.1 now')).toEqual([])
  })
})

describe('postScan and preRedact', () => {
  it('returns category, start, and end only, sorted and non-overlapping', () => {
    const text = 'a@b.co then b@c.co and 13800138000'
    const hits = postScan(text)
    expectShape(hits)
    expect(hits.map(hit => hit.category)).toEqual(['email', 'email', 'phone'])
    expect(hits[0].start).toBeLessThan(hits[1].start)
    expect(preRedact('')).toEqual({ text: '', counts: {} })
    expect(postScan('')).toEqual([])
  })

  it('redacts a mixed conversation to the fixed string', () => {
    const mixed = [
      '请把资料发到 ada@example.com ，电话 13800138000。',
      '脚本在 /Users/ada/bin/start.sh',
      '临时密钥 sk-abcdefghijklmnopqrst',
      '只连 10.8.8.8 这台机器',
    ].join('\n')
    const expected = [
      '请把资料发到 〔已脱敏·邮箱〕 ，电话 〔已脱敏·电话〕。',
      '脚本在 〔已脱敏·路径〕',
      '临时密钥 〔已脱敏·凭据〕',
      '只连 〔已脱敏·内网地址〕 这台机器',
    ].join('\n')
    const hits = postScan(mixed)
    expectShape(hits)
    let rebuilt = ''
    let cursor = 0
    for (const hit of hits) {
      rebuilt += mixed.slice(cursor, hit.start) + placeholder(hit.category)
      cursor = hit.end
    }
    rebuilt += mixed.slice(cursor)
    expect(rebuilt).toBe(expected)
    const out = preRedact(mixed)
    expect(out.text).toBe(expected)
    expect(out.counts).toEqual({
      email: 1,
      phone: 1,
      credential: 1,
      path: 1,
      internal_address: 1,
    })
    expect(postScan(out.text)).toEqual([])
  })

  it('throws from preRedact and reports the whole text from postScan when over the limit', () => {
    const big = 'a'.repeat(MAX_REDACT_INPUT + 1)
    expect(() => preRedact(big)).toThrow(RangeError)
    const hits = postScan(big)
    expect(hits).toEqual([{ category: 'credential', start: 0, end: big.length }])
    expect(Object.keys(hits[0]).sort()).toEqual(['category', 'end', 'start'])
  })

  it('finishes max-size inputs within 2 seconds', () => {
    const n = MAX_REDACT_INPUT
    const cases = [
      'a@'.repeat(n / 2),
      '1'.repeat(n),
      `/Users/${'x'.repeat(n - '/Users/'.length)}`,
      // Inputs that once took seconds to minutes: many near-misses, many hits.
      'eyJ'.repeat(Math.floor(n / 3)),
      'token sk-abcdefghijklmnop '.repeat(Math.floor(n / 26)),
      '~/'.repeat(n / 2),
      '-----BEGIN RSA PRIVATE KEY-----\n'.repeat(Math.floor(n / 32)),
      'Token: '.repeat(Math.floor(n / 7)),
      'a.corp '.repeat(Math.floor(n / 7)),
      '+86 '.repeat(n / 4),
    ]
    for (const input of cases) {
      expect(input.length).toBeLessThanOrEqual(n)
      let started = Date.now()
      const redacted = preRedact(input)
      expect(Date.now() - started).toBeLessThan(2000)
      expect(redacted.text.length).toBeGreaterThan(0)
      // Redacted text is clean, unless placeholders made it longer than the
      // limit: then the whole of it is held back, as any oversized text is.
      expect(postScan(redacted.text)).toEqual(redacted.text.length > n ? [{ category: 'credential', start: 0, end: redacted.text.length }] : [])
      started = Date.now()
      const hits = postScan(input)
      expect(Date.now() - started).toBeLessThan(2000)
      expect(Array.isArray(hits)).toBe(true)
    }
  }, 30000)
})
