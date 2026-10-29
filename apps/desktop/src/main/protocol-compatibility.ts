/**
 * Wire protocol this shell speaks. Must stay equal to
 * `agent24_protocol::PROTOCOL_VERSION`. Bump both by 1 when the wire changes
 * incompatibly (a field removed or renamed, a changed meaning, or a
 * difference a peer must refuse). Adding an optional field does not bump this.
 *
 * The decision table lives in `protocol/fixtures/protocol-compatibility.json`
 * and is executed by both this module and the Rust crate.
 */
export const CLIENT_PROTOCOL_VERSION = 1

export type ProtocolCompatibility =
  | { compatible: true }
  | { compatible: false; reason: string }

/** `null` / `undefined` is an old daemon that never wrote `protocol_version`. */
export function protocolCompatibility(
  daemon: number | null | undefined,
  client: number,
): ProtocolCompatibility {
  if (daemon === null || daemon === undefined) {
    return { compatible: false, reason: 'daemon 版本过旧，请升级' }
  }
  if (daemon === client) return { compatible: true }
  return {
    compatible: false,
    reason: `协议版本不兼容：daemon 协议版本 ${daemon}，客户端协议版本 ${client}`,
  }
}

/** `protocol_version` from a `/health` body. Missing or not a whole number is an old daemon. */
export function protocolVersionOf(body: unknown): number | null {
  if (typeof body !== 'object' || body === null) return null
  const value = (body as { protocol_version?: unknown }).protocol_version
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0) return null
  return value
}
