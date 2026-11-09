import { describe, expect, it } from 'vitest'
import { encodeLinkBrackets, restrictedMarkdownProblem, type MarkdownProblem } from './knowledge-entry-markdown'

// Ported in order from TestAgent24RestrictedMarkdown. problem is null when the
// Go check accepts. Rejects name the first hit in that function's scan order
// (raw "]:" / "][" over the line, then the prose scan) and its 1-based line.
type RestrictedCase =
  | { body: string; problem: null }
  | { body: string; problem: MarkdownProblem; line: number }

const cases: RestrictedCase[] = [
  { body: '', problem: null },
  { body: '# 标题\n\n## 二\n\n### 三\n\n段落 *强调* **粗** _x_', problem: null },
  { body: '1. a\n2. b\n\n- c\n- d\n\n> 引用', problem: null },
  { body: '见 [文档](https://example.org/a?b=1) 与 `inline <b>`', problem: null },
  { body: '[ref]: https://example.org/x', problem: null },
  { body: 'a < b and 1<2 and x <= y and <3', problem: null },
  { body: '&amp; &lt;b&gt; &#35;', problem: null },
  { body: 'C:\\Users\\name and \\d and a \\ b', problem: null },
  { body: '```\n<script>alert(1)</script>\n![x](y)\n```\nafter', problem: null },
  { body: '~~~~ go\n<b>\n~~~\n~~~~\nok', problem: null },
  // Indented fence is prose: the backtick run on line 1 has no closer.
  { body: '   ```\n<div>\n   ```', problem: 'unpaired_backtick', line: 1 },
  { body: '```\n<b>\n   ```\nok', problem: null },
  { body: '`` a ` <b> ``', problem: null },
  { body: 'unclosed ``` fence still prose?', problem: 'unpaired_backtick', line: 1 },
  { body: '```\nnever closed <div>', problem: null },
  { body: '<script>x</script>', problem: 'raw_html', line: 1 },
  { body: '</p>', problem: 'raw_html', line: 1 },
  { body: '<!-- c -->', problem: 'raw_html', line: 1 },
  { body: '<?php ?>', problem: 'raw_html', line: 1 },
  { body: '<Div>', problem: 'raw_html', line: 1 },
  { body: '<https://example.org>', problem: 'raw_html', line: 1 },
  { body: '![img](https://example.org/a.png)', problem: 'image', line: 1 },
  { body: 'text[^1]\n\n[^1]: note', problem: 'footnote', line: 1 },
  { body: 'text[^1] alone', problem: 'footnote', line: 1 },
  { body: '\\*not emphasis\\*', problem: 'escape', line: 1 },
  { body: 'hard break\\', problem: 'escape', line: 1 },
  { body: '[x](http://example.org)', problem: 'unsafe_link', line: 1 },
  { body: '[x](javascript:alert(1))', problem: 'unsafe_link', line: 1 },
  { body: '[x]( https://example.org)', problem: null },
  { body: '[x](HTTPS://EXAMPLE.ORG)', problem: null },
  { body: '[x](https://)', problem: 'unsafe_link', line: 1 },
  { body: '[x](https://example.org/`)<img src="https://attacker.example/p">`', problem: 'unsafe_link', line: 1 },
  { body: '[x](https://example.org "t`")<img src=x>`', problem: 'unsafe_link', line: 1 },
  { body: '[x](https://example.org "title")', problem: 'unsafe_link', line: 1 },
  { body: '[x](https://example.org/(a))', problem: 'unsafe_link', line: 1 },
  { body: '[x](<https://example.org>)', problem: 'unsafe_link', line: 1 },
  { body: '[x](https://example.org/`a`)', problem: 'unsafe_link', line: 1 },
  { body: '[x](https://a.example/b_%28c%29)', problem: null },
  { body: '    <b> in indented code', problem: 'raw_html', line: 1 },
  { body: '`x\n` <img src=x> `y`', problem: 'unpaired_backtick', line: 1 },
  { body: '- item\n  ```\n<img src=x>\n  ```', problem: 'unpaired_backtick', line: 2 },
  { body: '[ref]: https://example.org "t`"', problem: 'unsafe_link', line: 1 },
  { body: '[x]\n\n> [x]: javascript:alert(1)', problem: 'unsafe_link', line: 3 },
  { body: '[x]\n\n- [x]: http://attacker.example/', problem: 'unsafe_link', line: 3 },
  { body: '[x]\n\n1. > [x]: http://attacker.example/', problem: 'unsafe_link', line: 3 },
  // "[x" / "y]: http://..." — line 1 is prose with no sentinel; "]:" on line 2 is not https.
  { body: '[x\ny]: http://attacker.example/', problem: 'unsafe_link', line: 2 },
  { body: '[x]:\n  javascript:alert(1)', problem: 'unsafe_link', line: 1 },
  { body: '> [x]: https://example.org/ok', problem: null },
  { body: '注意 [x]: 不是链接', problem: 'unsafe_link', line: 1 },
  // Raw "]:" fails on the bracket in the destination before the prose scan sees "![".
  { body: 'a [x]: https://e.org![y]\n\n[y]: https://e.org/i', problem: 'unsafe_link', line: 1 },
  { body: '[x]: https://e.org[^1]', problem: 'unsafe_link', line: 1 },
  { body: '[x]: https://e.org/a [y]: javascript:x', problem: 'unsafe_link', line: 1 },
  { body: '](https://![x][y])\n\n[y]: https://evil.example/p.png', problem: 'unsafe_link', line: 1 },
  { body: '](https://![y])\n\n[y]: https://evil.example/p.png', problem: 'unsafe_link', line: 1 },
  { body: '[a](https://![x][y])', problem: 'unsafe_link', line: 1 },
  { body: '[a](https://e.org/[x])', problem: 'unsafe_link', line: 1 },
  { body: '[a](https://e.org/[^1])', problem: 'unsafe_link', line: 1 },
  { body: '[a](https://e.org/x]) ![y]', problem: 'unsafe_link', line: 1 },
  { body: '](https://e.org/x) t', problem: null },
  // Raw "]:" sees the http destination even though a backtick sits in front of it.
  { body: '[x`]: http://attacker.example/ "`"\n\n[go][x`]<img src=x>`', problem: 'unsafe_link', line: 1 },
  { body: '> [x`]: http://a.example/ "`"', problem: 'unsafe_link', line: 1 },
  { body: '[x`]: https://e.org/ok "`"', problem: 'unsafe_link', line: 1 },
  { body: '`[x]: http://a.example/`', problem: 'unsafe_link', line: 1 },
  { body: '[go][x`]<img src=x>`', problem: 'reference_label', line: 1 },
  { body: '[go][`x`] text', problem: 'reference_label', line: 1 },
  // Line 1 is "[go][x" — the "][" label is not closed on that line.
  { body: '[go][x\n`]<img src=x>`', problem: 'reference_label', line: 1 },
  { body: '[go][x] and [y][] and [z]', problem: null },
  { body: '[`code`](https://example.org/a) and [`x`]', problem: null },
  { body: '`a` and `b` and ``c`d``', problem: null },
  { body: '[ref]: http://example.org', problem: 'unsafe_link', line: 1 },
  { body: '`code` then <b>', problem: 'raw_html', line: 1 },
  { body: 'unmatched ` then <b>', problem: 'unpaired_backtick', line: 1 },
  { body: 'unmatched ` alone', problem: 'unpaired_backtick', line: 1 },
  { body: '    ```\n<b>\n    ```', problem: 'unpaired_backtick', line: 1 },
  { body: '```js`\n<b>\n```', problem: 'unpaired_backtick', line: 1 },
  { body: '~~~\n<b>\n```\n<i>', problem: null },
  { body: '```\na\n``\n<b>', problem: null },
  { body: '```\na\n```x\n<b>', problem: null },
  { body: '> ```\n> <b>\n> ```', problem: 'unpaired_backtick', line: 1 },
]

// TestAgent24MarkdownClosesFence, at the violation function: four spaces do not
// close, three spaces do, a longer run closes, and trailing space plus tab closes.
const fenceHelperCases: RestrictedCase[] = [
  { body: '```\na\n    ```\n<b>', problem: null },
  { body: '```\na\n   ```\n<b>', problem: 'raw_html', line: 4 },
  { body: '```\n<b>\n````\n<i>', problem: 'raw_html', line: 4 },
  { body: '```\n<b>\n```  \t\n<i>', problem: 'raw_html', line: 4 },
]

describe('restrictedMarkdownProblem', () => {
  it('ports every TestAgent24RestrictedMarkdown row', () => {
    expect(cases).toHaveLength(79)
  })

  for (const tc of cases) {
    it(JSON.stringify(tc.body), () => {
      const got = restrictedMarkdownProblem(tc.body)
      if (tc.problem === null) {
        expect(got).toBeNull()
      } else {
        expect(got).toEqual({ problem: tc.problem, line: tc.line })
      }
    })
  }

  for (const tc of fenceHelperCases) {
    it(`fence helper ${JSON.stringify(tc.body)}`, () => {
      const got = restrictedMarkdownProblem(tc.body)
      if (tc.problem === null) {
        expect(got).toBeNull()
      } else {
        expect(got).toEqual({ problem: tc.problem, line: tc.line })
      }
    })
  }
})

describe('encodeLinkBrackets', () => {
  it('encodes brackets in an inline https path and query', () => {
    const body = 'see [docs](https://example.org/a[b]/c?q=[d]&e=[f])'
    expect(encodeLinkBrackets(body)).toBe('see [docs](https://example.org/a%5Bb%5D/c?q=%5Bd%5D&e=%5Bf%5D)')
  })

  it('encodes brackets in a reference definition', () => {
    const body = '[ref]: https://example.org/a[b]?q=[c]'
    expect(encodeLinkBrackets(body)).toBe('[ref]: https://example.org/a%5Bb%5D?q=%5Bc%5D')
  })

  it('leaves brackets in link text unchanged', () => {
    expect(encodeLinkBrackets('[a[b]](https://example.org/c[d])')).toBe('[a[b]](https://example.org/c%5Bd%5D)')
  })

  it('leaves a link inside inline code unchanged', () => {
    const body = 'before `[a](https://example.org/a[b])` after'
    expect(encodeLinkBrackets(body)).toBe(body)
  })

  it('leaves a link inside a top-level fence unchanged', () => {
    const body = '```\n[a](https://example.org/a[b])\n[ref]: https://example.org/c[d]\n```'
    expect(encodeLinkBrackets(body)).toBe(body)
  })

  it('leaves an http destination unchanged', () => {
    const body = '[a](http://example.org/a[b]?q=[c])'
    expect(encodeLinkBrackets(body)).toBe(body)
  })

  it('is idempotent', () => {
    const bodies = [
      '[a[b]](https://example.org/c[d]?q=[e])\n[ref]: https://example.org/x[y]',
      'plain [docs]( https://example.org/a)',
      '```\n[a](https://example.org/a[b])\n```\n[c](https://example.org/c[d])',
    ]
    for (const body of bodies) {
      const once = encodeLinkBrackets(body)
      expect(encodeLinkBrackets(once), JSON.stringify(body)).toBe(once)
    }
    expect(encodeLinkBrackets(bodies[2])).toBe('```\n[a](https://example.org/a[b])\n```\n[c](https://example.org/c%5Bd%5D)')
  })

  it('encodes balanced parentheses in an inline destination', () => {
    expect(encodeLinkBrackets('[t](https://x/a(b)c)')).toBe('[t](https://x/a%28b%29c)')
  })

  it('encodes nested parentheses in an inline destination', () => {
    expect(encodeLinkBrackets('[t](https://x/a((b)))')).toBe('[t](https://x/a%28%28b%29%29)')
  })

  it('leaves an inline destination with unpaired parentheses unchanged', () => {
    const body = '[t](https://x/a(b)'
    expect(encodeLinkBrackets(body)).toBe(body)
    expect(restrictedMarkdownProblem(body)).toEqual({ problem: 'unsafe_link', line: 1 })
  })

  it('stops at whitespace in an inline destination: a title is never rewritten', () => {
    for (const line of ['[x](https://a/(b) "t(1)")', '[t](https://x/(a) b)', '[t](https://x/[a]\t)']) {
      expect(encodeLinkBrackets(line)).toBe(line)
      expect(restrictedMarkdownProblem(line)).toMatchObject({ problem: 'unsafe_link', line: 1 })
    }
  })

  it('passes the check after encoding parentheses', () => {
    for (const body of ['[t](https://x/a(b)c)', '[t](https://x/a((b)))']) {
      expect(restrictedMarkdownProblem(body), body).toEqual({ problem: 'unsafe_link', line: 1 })
      expect(restrictedMarkdownProblem(encodeLinkBrackets(body)), body).toBeNull()
    }
  })

  it('encoding parentheses twice is stable', () => {
    for (const body of ['[t](https://x/a(b)c)', '[t](https://x/a((b)))', '[ref]: https://x/a(b)[c]']) {
      const once = encodeLinkBrackets(body)
      expect(encodeLinkBrackets(once), body).toBe(once)
    }
  })

  it('encodes all four brackets in a reference destination and stops at whitespace', () => {
    expect(encodeLinkBrackets('[ref]: https://x/a(b)[c]')).toBe('[ref]: https://x/a%28b%29%5Bc%5D')
    expect(restrictedMarkdownProblem(encodeLinkBrackets('[ref]: https://x/a(b)[c]'))).toBeNull()
    expect(encodeLinkBrackets('[ref]: https://x/a(b) tail(c)')).toBe('[ref]: https://x/a%28b%29 tail(c)')
  })

  it('passes the check after encoding when brackets were the only problem', () => {
    const body = 'see [docs](https://example.org/a[b]/c?x=[y])\n[ref]: https://example.org/u[v]'
    expect(restrictedMarkdownProblem(body)).toEqual({ problem: 'unsafe_link', line: 1 })
    const encoded = encodeLinkBrackets(body)
    expect(encoded).toBe('see [docs](https://example.org/a%5Bb%5D/c?x=%5By%5D)\n[ref]: https://example.org/u%5Bv%5D')
    expect(restrictedMarkdownProblem(encoded)).toBeNull()
    const upper = '[a](HTTPS://example.org/a[b])'
    expect(encodeLinkBrackets(upper)).toBe('[a](HTTPS://example.org/a%5Bb%5D)')
    expect(restrictedMarkdownProblem(encodeLinkBrackets(upper))).toBeNull()
  })
})

describe('non-ASCII and emoji', () => {
  it('does not treat non-ASCII scalars as sentinels', () => {
    expect(restrictedMarkdownProblem('见 [文档](https://example.org/路径) 🙂')).toBeNull()
    expect(restrictedMarkdownProblem('a \\🙂 b')).toBeNull()
    expect(restrictedMarkdownProblem('<🙂>')).toBeNull()
  })

  it('still sees ASCII sentinels after non-ASCII scalars', () => {
    expect(restrictedMarkdownProblem('🙂 <b>')).toEqual({ problem: 'raw_html', line: 1 })
    expect(restrictedMarkdownProblem('字\\*')).toEqual({ problem: 'escape', line: 1 })
    expect(restrictedMarkdownProblem('🌿![x]')).toEqual({ problem: 'image', line: 1 })
  })

  it('encodes brackets beside an emoji and then passes', () => {
    const body = '[a](https://example.org/🙂[b])'
    expect(restrictedMarkdownProblem(body)).toEqual({ problem: 'unsafe_link', line: 1 })
    const encoded = encodeLinkBrackets(body)
    expect(encoded).toBe('[a](https://example.org/🙂%5Bb%5D)')
    expect(restrictedMarkdownProblem(encoded)).toBeNull()
    expect(encodeLinkBrackets(encoded)).toBe(encoded)
  })
})
