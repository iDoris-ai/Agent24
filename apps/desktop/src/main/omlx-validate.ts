// A4 (pre-release desktop hardening): the renderer must not be able to point
// the main process (which attaches the user's oMLX API key) at an arbitrary
// host. Everything oMLX-bound goes through these two checks first.

const LOOPBACK_HOSTS = new Set(['127.0.0.1', 'localhost', '[::1]', '::1'])

/** `input` as a URL iff it is http(s), credential-free and loopback-only. */
export function parseLoopbackUrl(input: unknown): URL | null {
  if (typeof input !== 'string' || input === '') return null
  let url: URL
  try {
    url = new URL(input)
  } catch {
    return null
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') return null
  if (url.username !== '' || url.password !== '') return null
  if (!LOOPBACK_HOSTS.has(url.hostname)) return null
  return url
}

/** A TCP port: a real number (not a numeric string), integer, 1..65535. */
export function parsePort(input: unknown): number | null {
  return typeof input === 'number' && Number.isInteger(input) && input >= 1 && input <= 65535 ? input : null
}

/** `<origin><base path>/<suffix>` for a validated URL (query/hash dropped). */
export function omlxEndpoint(base: URL, suffix: string): URL {
  return new URL(`${base.origin}${base.pathname.replace(/\/+$/, '')}${suffix}`)
}
