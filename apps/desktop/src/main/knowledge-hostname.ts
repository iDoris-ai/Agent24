// The host names Knowledge accepts for its server (#1643 PR-Daemon C). The
// WHATWG URL parser keeps `*`, `,`, `;` and the like in a host name, and the
// KnowledgeView's proxy bypass list is built from it: `*` there would send
// every connection direct (contract decision #30). So a host name is only
// DNS labels of letters, digits and hyphens (punycode included), an IPv4
// literal, or a bracketed IPv6 literal.

const LABEL = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/
const IPV4 = /^(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)$/
const IPV6 = /^\[[0-9a-f:.]+\]$/

/** `hostname` as `URL` gives it (lower case, IPv6 in brackets). */
export function isPlainHostname(hostname: string): boolean {
  if (IPV4.test(hostname) || IPV6.test(hostname)) return true
  // One trailing root dot is a fully qualified name (`kb.example.org.`); it is
  // only set aside here, the URL and its origin keep it.
  const name = hostname.endsWith('.') ? hostname.slice(0, -1) : hostname
  if (name.length === 0 || name.length > 253) return false
  return name.split('.').every((label) => LABEL.test(label))
}
