// E3: validate a community module's manifest against protocol/module.schema.json
// at install/load time. Grounds the "UI Module spec" and is the gate H10's
// install-consent flow builds on — a package that ships a malformed manifest is
// rejected before it can register anything.
//
// A focused, dependency-free validator: it enforces the schema's STRUCTURAL
// contract (required fields present, correct JSON types, the id pattern) but
// deliberately does NOT reject unknown enum values or unknown fields — the
// schema declares its string enums as OPEN sets for forward compatibility and
// requires consumers to ignore unknown fields. The `module-manifest.test.ts`
// drift-guard pins these rules to module.schema.json so they can't silently
// diverge from the authority.

/** Fields module.schema.json marks `required`. Pinned to the schema by the drift test. */
export const REQUIRED_FIELDS = [
  'id',
  'version',
  'name',
  'description',
  'type',
  'permissions',
] as const

/** The module-id pattern — identical to module.schema.json `properties.id.pattern`
 * and module-installer's `isValidPackageName` (a plain or scoped npm name). */
export const ID_PATTERN = /^(@[a-z0-9-~][a-z0-9-._~]*\/)?[a-z0-9-~][a-z0-9-._~]*$/

const STRING_FIELDS = ['id', 'version', 'name', 'description', 'type'] as const

/** Returns a list of human-readable problems; empty means the manifest conforms
 * to the structural contract of module.schema.json. */
export function validateManifest(manifest: unknown): string[] {
  if (typeof manifest !== 'object' || manifest === null || Array.isArray(manifest)) {
    return ['manifest must be a JSON object']
  }
  const m = manifest as Record<string, unknown>
  const errors: string[] = []

  for (const field of REQUIRED_FIELDS) {
    if (!(field in m)) errors.push(`missing required field: ${field}`)
  }
  for (const field of STRING_FIELDS) {
    if (field in m && typeof m[field] !== 'string') {
      errors.push(`field "${field}" must be a string`)
    }
  }
  if (typeof m.id === 'string' && !ID_PATTERN.test(m.id)) {
    errors.push(`id "${m.id}" is not a valid module id (must be a plain or scoped npm package name)`)
  }
  if ('permissions' in m) {
    if (!Array.isArray(m.permissions)) {
      errors.push('permissions must be an array')
    } else if (!m.permissions.every((p) => typeof p === 'string')) {
      errors.push('every permission must be a string')
    }
  }
  return errors
}

/** The user-facing summary shown before a freshly installed module is enabled
 * (H10). It is the module's own declaration of what it wants — the user decides
 * whether to trust it. Assumes `manifest` already passed {@link validateManifest}. */
export interface ConsentSummary {
  id: string
  name: string
  type: string
  /** What the module declares it needs access to (llm, network, filesystem, …). */
  permissions: string[]
}

export function consentSummary(manifest: Record<string, unknown>): ConsentSummary {
  const permissions = Array.isArray(manifest.permissions)
    ? manifest.permissions.filter((p): p is string => typeof p === 'string')
    : []
  return {
    id: String(manifest.id ?? ''),
    name: String(manifest.name ?? manifest.id ?? ''),
    type: String(manifest.type ?? ''),
    permissions,
  }
}
