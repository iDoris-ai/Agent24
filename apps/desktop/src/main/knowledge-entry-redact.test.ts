import { describe, expect, it } from 'vitest'
import {
  MAX_REDACT_INPUT,
  placeholder,
  postScan,
  preRedact,
  testing,
  type RedactionCategory,
  type RedactionHit,
} from './knowledge-entry-redact'

const { tableScan } = testing

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
  const hits = tableScan(text)
  expectShape(hits)
  return hits.map(hit => hit.category)
}

function redact(text: string): string {
  const out = preRedact(text)
  expect(tableScan(out.text)).toEqual([])
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
    expect(tableScan(raw)).toEqual([])
    expect(preRedact(raw)).toEqual({ text: raw, counts: {} })
    expect(tableScan(preRedact(`password=${placeholder('credential')}`).text)).toEqual([])
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
    const hits = tableScan(text)
    expectShape(hits)
    expect(hits).toHaveLength(1)
    expect(text.slice(hits[0].start, hits[0].end)).toBe('-----BEGIN EC PRIVATE KEY-----')
    expect(redact(text)).toBe('〔已脱敏·私钥〕\nkeep this line\nstill here\n')
  })

  it('does not match the lines that follow a BEGIN with no END', () => {
    const text = '-----BEGIN PRIVATE KEY-----\nnot a key body\n'
    const hits = tableScan(text)
    expect(hits).toHaveLength(1)
    expect(text.slice(hits[0].end)).toBe('\nnot a key body\n')
    expect(categories('-----BEGIN PUBLIC KEY-----\nAAA\n-----END PUBLIC KEY-----')).toEqual([])
  })
})

describe('credential', () => {
  it('takes a JWT only with all three segments, and the whole of it', () => {
    expect(redact(`x ${JWT} y`)).toBe('x 〔已脱敏·凭据〕 y')
    // Two segments and a third word that is not joined by a dot.
    expect(categories('eyJabcd.eyJefgh ijklmnop')).toEqual([])
    expect(categories('eyJabcd.eyJefgh_ijklmnop')).toEqual([])
    // Each segment needs at least four characters.
    expect(categories('eyJabc.eyJdefg.hijk')).toEqual([])
    expect(categories('eyJabcd.eyJefgh.ijk')).toEqual([])
    // A run of eyJ before a real one does not hide it.
    expect(redact(`eyJeyJeyJ ${JWT}`)).toBe('eyJeyJeyJ 〔已脱敏·凭据〕')
  })

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
    const [hit] = tableScan(text)
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

  it('does not match text without @, or without a name before it', () => {
    expect(categories('ada.example.com')).toEqual([])
    expect(categories('write to @example.com')).toEqual([])
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

  it('matches when a letter is glued to the number, not a digit (PR-Daemon B5)', () => {
    expect(categories(`A${VALID_IDS[0]}`)).toEqual(['id_card'])
    expect(categories(`${VALID_IDS[0]}B`)).toEqual(['id_card'])
    expect(categories(`1${VALID_IDS[0]}`)).not.toContain('id_card')
    expect(categories(`${VALID_IDS[0]}1`)).not.toContain('id_card')
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
    const hits = tableScan(`证件 ${DUAL_ID} 在此`)
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
    const hits = tableScan(text)
    expectShape(hits)
    expect(hits.map(hit => hit.category)).toEqual(['email', 'email', 'phone'])
    expect(hits[0].start).toBeLessThan(hits[1].start)
    expect(preRedact('')).toEqual({ text: '', counts: {} })
    expect(tableScan('')).toEqual([])
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
    const hits = tableScan(mixed)
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
    expect(tableScan(out.text)).toEqual([])
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
      expect(tableScan(redacted.text)).toEqual(redacted.text.length > n ? [{ category: 'credential', start: 0, end: redacted.text.length }] : [])
      started = Date.now()
      const hits = tableScan(input)
      expect(Date.now() - started).toBeLessThan(2000)
      expect(Array.isArray(hits)).toBe(true)
    }
  }, 30000)
})

// Codex round 1 on #1669: each case once left a secret, or took seconds.
describe('nothing hides a secret from another rule', () => {
  it('a secret next to a placeholder, or to half of one, is taken (C1)', () => {
    for (const text of [
      `password=TOPSECRET${placeholder('credential')}`,
      'password=TOPSECRET〔已脱敏·',
      `${placeholder('email')}sk-abcdefghijklmnopqrst`,
      `${placeholder('phone')}13800138000`,
    ]) {
      const out = redact(text)
      expect(out, text).not.toMatch(/TOPSECRET|abcdefghijklmnop|13800138000/)
    }
  })

  it('a value whose key sits inside another hit is still taken (C2)', () => {
    expect(redact('sk-abcdefghijklmnoppassword=TOPSECRET')).not.toContain('TOPSECRET')
    expect(redact('password=sk-aaaaaaaaaaaaaaaaaaaa,hunter2')).toBe('password=〔已脱敏·凭据〕')
    expect(redact('ada@example.com,token=hunter2')).not.toContain('hunter2')
  })

  it('overlapping hits become one span with the earlier rule’s category', () => {
    const text = 'see /Users/ada@example.com now'
    const hits = tableScan(text)
    expect(hits).toEqual([{ category: 'email', start: 4, end: 26 }])
    expect(redact(text)).toBe('see 〔已脱敏·邮箱〕 now')
  })

  it('takes the whole of a long token, path or value, not a prefix of it', () => {
    expect(redact(`k sk-${'a'.repeat(300)} k`)).toBe('k 〔已脱敏·凭据〕 k')
    expect(redact(`k /Users/${'a'.repeat(1000)} k`)).toBe('k 〔已脱敏·路径〕 k')
    expect(redact(`password=${'a'.repeat(1000)} k`)).toBe('password=〔已脱敏·凭据〕 k')
    expect(redact(`Bearer ${'a'.repeat(500)} k`)).toBe('〔已脱敏·凭据〕 k')
  })

  it('takes a quoted value whole, spaces included', () => {
    expect(redact('{"password": "hunter 2 x", "user": "ada"}')).toBe('{"password": 〔已脱敏·凭据〕, "user": "ada"}')
    expect(redact("secret='a b c'")).toBe('secret=〔已脱敏·凭据〕')
  })

  it('a value of punctuation alone is a value, not something already redacted', () => {
    expect(redact('password=.;.;')).toBe('password=〔已脱敏·凭据〕')
    expect(redact('pwd: ")"')).toBe('pwd: 〔已脱敏·凭据〕')
  })
})

describe('private_key blocks (C3, C4)', () => {
  const body = ['MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7', 'VJTUt9Us8cKjMzEfYyjiWA4R4/M2bS1GB4t7NXp98C3SC6dVMvDuictGeurT8jNbvJZHtCSuYEvu', 'NMoSfm76oqFvAp8Gy0iz5sxjZmSnXyCdPEovGhLa0VzMaQ8s+CLOyS56YyCFGeJZ']

  it('ends only at an END of the same type', () => {
    const text = ['-----BEGIN RSA PRIVATE KEY-----', body[0], '-----END EC PRIVATE KEY-----', body[1], '-----END RSA PRIVATE KEY-----', 'after'].join('\n')
    expect(redact(text)).toBe('〔已脱敏·私钥〕\nafter')
  })

  it('without an END, takes every line that reads as key material, headers included', () => {
    const text = ['-----BEGIN PRIVATE KEY-----', ...body, '', 'Thanks, that was the key.'].join('\n')
    expect(redact(text)).toBe('〔已脱敏·私钥〕\n\nThanks, that was the key.')
    const encrypted = ['-----BEGIN RSA PRIVATE KEY-----', 'Proc-Type: 4,ENCRYPTED', 'DEK-Info: AES-128-CBC,3F17F5316E2BAC89', '', ...body, 'done here'].join('\n')
    expect(redact(encrypted)).toBe('〔已脱敏·私钥〕\ndone here')
  })

  it('takes a key pasted on one line', () => {
    expect(redact(`k -----BEGIN PRIVATE KEY----- ${body[0]} ${body[1]} -----END PRIVATE KEY----- k`)).toBe('k 〔已脱敏·私钥〕 k')
    expect(redact(`k -----BEGIN PRIVATE KEY----- ${body[0]} ${body[1]}`)).toBe('k 〔已脱敏·私钥〕')
  })
})

describe('whitespace and case (C5, C6)', () => {
  it('reads whitespace as JavaScript’s \\s does', () => {
    for (let k = 0; k <= 0xffff; k += 1) {
      const c = String.fromCharCode(k)
      expect(testing.isSpace(c), k.toString(16)).toBe(/\s/.test(c))
    }
  })

  it('a path after a no-break space, or after one that ends a URL, is taken', () => {
    expect(redact('see\u00a0/Users/ada/x')).toBe('see\u00a0〔已脱敏·路径〕')
    expect(redact('https://a.com\u00a0/Users/ada/x')).toBe('https://a.com\u00a0〔已脱敏·路径〕')
    expect(redact('https://a.com\u3000/Users/ada/x')).toBe('https://a.com\u3000〔已脱敏·路径〕')
    expect(redact('/Users/ada/x\u00a0after')).toBe('〔已脱敏·路径〕\u00a0after')
  })

  it('takes host suffixes and localhost in any case', () => {
    expect(redact('go db.INTERNAL now')).toBe('go 〔已脱敏·内网地址〕 now')
    expect(redact('go Nas.Local now')).toBe('go 〔已脱敏·内网地址〕 now')
    expect(redact('go LOCALHOST:3000 now')).toBe('go 〔已脱敏·内网地址〕 now')
  })
})

describe('separators (C7, C8)', () => {
  it('takes mobiles with a separator anywhere and international numbers in groups', () => {
    expect(redact('call 1-3800138000 now')).toBe('call 〔已脱敏·电话〕 now')
    expect(redact('call 1 380 0138 000 now')).toBe('call 〔已脱敏·电话〕 now')
    expect(redact('call +1 415-555-2671 now')).toBe('call 〔已脱敏·电话〕 now')
    expect(redact('call +44 20 7946 0958 now')).toBe('call 〔已脱敏·电话〕 now')
    expect(redact('call +1 (415) 555.2671 now')).toBe('call 〔已脱敏·电话〕 now')
    // Not a number with a dot: an address.
    expect(categories('go 172.31.255.255 now')).toEqual(['internal_address'])
  })

  it('takes cards with mixed separators, other groupings, and a number after them', () => {
    expect(redact('卡 4111 1111-1111 1111 结束')).toBe('卡 〔已脱敏·银行卡号〕 结束')
    expect(redact('卡 3782 822463 10005 结束')).toBe('卡 〔已脱敏·银行卡号〕 结束')
    expect(redact('卡 4111 1111 1111 1111 2026 结束')).toBe('卡 〔已脱敏·银行卡号〕 2026 结束')
    expect(redact('卡 6222 0212 3456 7890 128 结束')).not.toContain('6222')
  })

  it('does not read a list of small numbers as a card', () => {
    expect(categories('1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19')).toEqual([])
  })
})

// Codex round 2 on #1669.
describe('round 2: what one hit used to hide', () => {
  it('a key block runs to its END whatever lines it holds (C1)', () => {
    const text = '-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIAAAAAAAAAAA AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n-----END PRIVATE KEY-----\nafter'
    expect(redact(text)).toBe('〔已脱敏·私钥〕\nafter')
    const prose = '-----BEGIN RSA PRIVATE KEY-----\nThe body is below, after a note.\nMIIBogIBAAJB\n-----END RSA PRIVATE KEY-----'
    expect(redact(prose)).toBe('〔已脱敏·私钥〕')
  })

  it('without an END, a body line of several long base64 words is key material, prose is not', () => {
    expect(redact('-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIAAAAAAAAAAA AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\nok then')).toBe('〔已脱敏·私钥〕\nok then')
    expect(redact('-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIAAAAAAAAAAA and more\nok')).toBe('〔已脱敏·私钥〕\nMC4CAQAwBQYDK2VwBCIEIAAAAAAAAAAA and more\nok')
  })

  it('a quoted value runs past escaped quotes (C2)', () => {
    expect(redact('{"password":"abc\\" TOPSECRET tail"}')).toBe('{"password":〔已脱敏·凭据〕}')
    expect(redact("secret='it\\'s TOPSECRET'")).toBe('secret=〔已脱敏·凭据〕')
  })

  it('a later key inside an earlier value still has its own value taken (C3)', () => {
    expect(redact('password=one,password="two TOPSECRET"')).not.toContain('TOPSECRET')
    expect(redact('token=a,token="b c TOPSECRET" end')).toBe('token=〔已脱敏·凭据〕 end')
  })

  it('a Bearer, JWT or address that starts inside another and runs past it is taken', () => {
    expect(redact(`Bearer ${'a'.repeat(17)}bearer TOPSECRETTOPSECRET1 end`)).toBe('〔已脱敏·凭据〕 end')
    expect(redact('x eyJaaaa.eyJbbbb.cccceyJdddd.eyJeeee.TOPSECRET end')).toBe('x 〔已脱敏·凭据〕 end')
    expect(redact('go 10.0.0.1:192.168.0.1 now')).toBe('go 〔已脱敏·内网地址〕 now')
  })

  it('a chain of glued hits longer than the passes stays held back, never let through (C4, known limit)', () => {
    const short = preRedact('localhost:1'.repeat(3)).text
    expect(tableScan(short)).toEqual([])
    const long = preRedact('localhost:1'.repeat(8)).text
    expect(tableScan(long).length).toBeGreaterThan(0)
  })
})

// Codex round 3 on #1669.
describe('round 3: a block or JWT that starts inside another', () => {
  it('a key block of another type that begins inside one and ends past it is taken (C1)', () => {
    const text = ['-----BEGIN PRIVATE KEY-----', '-----BEGIN RSA PRIVATE KEY-----', 'A'.repeat(32), '-----END PRIVATE KEY-----', 'B'.repeat(32), '-----END RSA PRIVATE KEY-----', 'after'].join('\n')
    expect(redact(text)).toBe('〔已脱敏·私钥〕\nafter')
  })

  it('two blocks of the same type one after the other are each taken to their own END', () => {
    const block = (body: string) => ['-----BEGIN RSA PRIVATE KEY-----', body, '-----END RSA PRIVATE KEY-----'].join('\n')
    const text = `${block('A'.repeat(32))}\nmiddle\n${block('B'.repeat(32))}\nafter`
    // In one scan, not only after preRedact has taken the first block away.
    expect(categories(text)).toEqual(['private_key', 'private_key'])
    expect(redact(text)).toBe('〔已脱敏·私钥〕\nmiddle\n〔已脱敏·私钥〕\nafter')
  })

  it('a JWT that starts in the second segment of another is taken (C2)', () => {
    expect(redact('eyJaaaa.eyJbbbb.eyJcccc.TOPSECRET end')).toBe('〔已脱敏·凭据〕 end')
    expect(redact(`eyJxxxx.${JWT} end`)).toBe('〔已脱敏·凭据〕 end')
  })
})

describe('linear time on adversarial input (C9–C11)', () => {
  const n = MAX_REDACT_INPUT
  const fill = (unit: string, tail = ''): string => unit.repeat(Math.floor((n - tail.length) / unit.length)) + tail
  const cases: Array<[string, string]> = [
    ['one JWT through a long run (C9)', fill('sk-eyJa', '.eyJabcd.abcd')],
    ['BEGINs, then ENDs of another type (C10)', `${'-----BEGIN RSA PRIVATE KEY-----\n'.repeat(8000)}${'-----END EC PRIVATE KEY-----\n'.repeat(8000)}`],
    ['many hits (C11)', fill('a@b.co ')],
    ['quoted values never closed', fill('pwd="')],
    ['quoted values on one line', fill('pwd="x ')],
    ['UNC near-misses', fill('\\\\a')],
    ['paths inside URLs', fill('http://x/Users/')],
    ['card groups', fill('123 ')],
    ['phone groups', fill('+1 ')],
    ['one value', `password=${'x'.repeat(n - 9)}`],
    ['JWT near-misses', fill('eyJaaaa.')],
    ['one-line key bodies', fill('-----BEGIN PRIVATE KEY----- AAAA ')],
    ['hosts', fill('a.lan.')],
    ['placeholders', fill(placeholder('credential'))],
    // Round 2: finders that now look inside their own hits.
    ['keys inside one value', fill('pwd=')],
    ['keys inside one quoted value', `pwd="${fill('pwd="x ', '"').slice(5)}`],
    ['escaped quotes', fill('pwd="\\"')],
    ['Bearers inside a Bearer', `Bearer ${fill('bearerx').slice(7)}`],
    ['chained JWTs', fill('eyJaaaa.')],
    ['one BEGIN, many ENDs', `-----BEGIN PRIVATE KEY-----\n${fill('-----END PRIVATE KEY-----\n').slice(28)}`],
    ['BEGINs of many types, no END', fill('-----BEGIN A B C PRIVATE KEY-----\nMIIB MIIB\n')],
    ['BEGINs inside one closed block', `${fill('-----BEGIN PRIVATE KEY-----\n', '-----END PRIVATE KEY-----')}`],
    ['BEGINs on one header line', `-----BEGIN PRIVATE KEY-----\nX: ${fill('-----BEGIN PRIVATE KEY----- ').slice(31)}`],
  ]
  it.each(cases)('%s', (_name, input) => {
    expect(input.length).toBeLessThanOrEqual(n)
    let started = Date.now()
    tableScan(input)
    expect(Date.now() - started).toBeLessThan(2000)
    started = Date.now()
    const out = preRedact(input)
    expect(Date.now() - started).toBeLessThan(4000)
    expect(tableScan(out.text)).toEqual(out.text.length > n ? [{ category: 'credential', start: 0, end: out.text.length }] : [])
  }, 30000)
})

describe('preRedact output scans clean', () => {
  const pieces = [
    'password=', 'Token: ', '"secret": "', 'sk-', 'abcdefghijklmnopqrstu', placeholder('credential'), placeholder('path'), '〔已脱敏·',
    ' ', '\n', '\u00a0', '@', 'ex.com', '1380013', '8000', '+44 20 ', '4111 1111 ', '/Users/', '~/', 'C:\\', '\\\\srv\\',
    'eyJ', 'abcd.', '.', '"', "'", ':', '=', '-----BEGIN RSA PRIVATE KEY-----', '-----END RSA PRIVATE KEY-----', 'MIIB',
    'db.INTERNAL', '10.0.0.1', ':8080', 'localhost:', 'https://', '，', '。', '\\"', 'Bearer ', 'eyJabcd.eyJ',
  ]
  it('for random mixes of the tricky parts', () => {
    let seed = 1669
    const random = (): number => {
      seed = (seed * 1103515245 + 12345) % 2147483648
      return seed / 2147483648
    }
    for (let round = 0; round < 3000; round += 1) {
      let text = ''
      const count = 1 + Math.floor(random() * 14)
      for (let i = 0; i < count; i += 1) text += pieces[Math.floor(random() * pieces.length)]
      const out = preRedact(text)
      expect(tableScan(out.text), JSON.stringify(text)).toEqual([])
      expectShape(tableScan(text))
    }
  })
})

// PR-Daemon round 1 on #1677 (head 8d00bd36): each bypass it ran, now redacted.
describe('PR-Daemon round 1', () => {
  const leaks = (text: string, secret: string): void => {
    const out = preRedact(text).text
    expect(out, JSON.stringify(text)).not.toContain(secret)
    expect(postScan(text).length, JSON.stringify(text)).toBeGreaterThan(0)
  }

  it('B1: takes the value after any run of spaces around the separator', () => {
    leaks('{"password":    "hunter2"}', 'hunter2')
    leaks('password      =      hunter2', 'hunter2')
    leaks(`password:${' '.repeat(40)}hunter2`, 'hunter2')
    leaks('password:\n  hunter2', 'hunter2')
  })

  it('B2: takes a quoted value across lines, and the next word after a quote that never closes', () => {
    leaks('password="\nhunter2"', 'hunter2')
    leaks('password="\nhunter2', 'hunter2')
    leaks("password='first\nsecond-part'", 'second-part')
    leaks('password="  hunter2', 'hunter2')
  })

  it('B2: a quote that never closes on a long line stays linear', () => {
    const text = 'pwd="'.repeat(100_000)
    const started = Date.now()
    preRedact(text)
    postScan(text)
    expect(Date.now() - started).toBeLessThan(4000)
  }, 30000)

  it('B3: finds a key inside a longer name, and authorization, passphrase, private_key', () => {
    leaks('SECRET_KEY=abc123def456', 'abc123def456')
    leaks('db_password: s3cr3tvalue', 's3cr3tvalue')
    leaks('private_key=MIIEvQIBADANBgkqhki', 'MIIEvQIBADANBgkqhki')
    leaks('passphrase: correct-horse', 'correct-horse')
    leaks('client_secret = zzz999', 'zzz999')
    leaks('Authorization: Basic dXNlcjpwYXNz', 'dXNlcjpwYXNz')
  })

  it('B4: redacts file: URLs and Windows paths that follow a URL', () => {
    leaks('open file:///Users/alice/notes.txt', 'alice')
    leaks('open file:///C:/Users/alice/notes.txt', 'alice')
    leaks('saved to D:/Projects/alice/plan.txt', 'alice')
    leaks('open file:///D:/Projects/alice/plan.txt', 'alice')
    leaks('open file://localhost/Users/alice/notes.txt', 'alice')
    leaks('https://example.com,C:\\Users\\alice\\secret.txt', 'alice')
    leaks('see https://example.com/x \\\\fileserver\\share\\alice', 'alice')
  })

  it('B4: still leaves the path part of an ordinary URL to the table', () => {
    expect(preRedact('https://example.com/var/log').text).toBe('https://example.com/var/log')
  })

  it('B5: redacts an id number glued to letters, not one glued to digits', () => {
    leaks(`ID${VALID_IDS[0]}`, VALID_IDS[0])
    leaks(`${VALID_IDS[0]}abc`, VALID_IDS[0])
    expect(tableScan(`9${VALID_IDS[0]}`).map((h) => h.category)).not.toContain('id_card')
  })

  it('B6: postScan holds back shapes the table does not replace', () => {
    const shapes: Array<[string, RedactionCategory]> = [
      ['order 123456789012345 shipped', 'bank_card'],
      [`ref${VALID_IDS[0].slice(0, 17)}9z`, 'id_card'],
      ['https://example.com/home/alice/', 'path'],
      ['https://example.com/Users/alice', 'path'],
      ['see file:/srv/data/x', 'path'],
    ]
    for (const [text, category] of shapes) {
      expect(tableScan(text).map((h) => h.category), text).not.toContain(category)
      expect(postScan(text).map((h) => h.category), text).toContain(category)
    }
  })

  it('B6: postScan never finds less than the table', () => {
    const text = `password=hunter2 a@b.co ${VISA} /Users/x 10.0.0.1 https://example.com/home/y 12345678901234567`
    const table = tableScan(text)
    const wide = postScan(text)
    for (const hit of table) expect(wide.some((w) => w.start <= hit.start && w.end >= hit.end)).toBe(true)
    expect(wide.length).toBeGreaterThan(0)
  })

  it('B6: profile: is not a file: URL', () => {
    expect(postScan('profile:/x')).toEqual([])
  })

  it('N4: internal addresses at the end of a sentence, root dots, more private ranges, IPv6', () => {
    leaks('See 10.0.0.1.', '10.0.0.1')
    leaks('https://db.corp./x', 'db.corp')
    leaks('link 169.254.10.20 here', '169.254.10.20')
    leaks('tailnet 100.66.210.41 here', '100.66.210.41')
    leaks('ula fd12:3456:789a::1 here', 'fd12:3456:789a::1')
    leaks('link fe80::1ff:fe23:4567:890a here', 'fe80::1ff:fe23:4567:890a')
    expect(tableScan('public 100.20.1.1 and 8.8.8.8')).toEqual([])
  })

  it('N4: the password in a URL with a user', () => {
    leaks('postgres://u:pw1234@host/db', 'pw1234')
    leaks('redis://:topsecret@cache:6379', 'topsecret')
  })
})

