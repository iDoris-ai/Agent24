// Read-only client for GET /api/v1/components. The host report POST is not
// referenced here: main calls it with the ProductHost credential, and the
// proxy allowlist does not forward it.

import { isComponentState, type ComponentState } from '../../shared/component-state'

export interface ComponentProvide {
  id: string
}

/**
 * Full or redacted row. `form` and `reason` are present only on the full view:
 * a ProductHost audience, and legacy mode when the request carries no finer
 * audience. A redacted audience omits both. `reason` is also absent when the
 * daemon has none.
 */
export interface ComponentDeclared {
  kernel: string[]
  operations: string[]
}

export interface ComponentView {
  id: string
  version: string | null
  state: ComponentState
  form?: string
  provides: ComponentProvide[]
  reason?: string
  /** Present on the host view. What the mount actually handed out. */
  kernel_grants?: string[]
  /** Present on the host view. The component's requires declaration. */
  declared?: ComponentDeclared
}

export class ComponentApiError extends Error {
  constructor(readonly code: 'backend_not_ready' | 'request_failed' | 'invalid_response') {
    super(code)
  }
}

const FORMS = ['resident', 'sidecar', 'cli', 'attach', 'remote', 'mcp'] as const

function object(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) throw new ComponentApiError('invalid_response')
  return value as Record<string, unknown>
}

function parseProvide(value: unknown): ComponentProvide {
  const row = object(value)
  if (typeof row.id !== 'string' || row.id.length === 0) throw new ComponentApiError('invalid_response')
  return { id: row.id }
}

function parseStringList(value: unknown): string[] {
  if (!Array.isArray(value) || value.some((item) => typeof item !== 'string')) {
    throw new ComponentApiError('invalid_response')
  }
  return value as string[]
}

export function parseComponentView(value: unknown): ComponentView {
  const row = object(value)
  if (typeof row.id !== 'string' || row.id.length === 0 || !isComponentState(row.state)) {
    throw new ComponentApiError('invalid_response')
  }
  if (row.version !== null && typeof row.version !== 'string') throw new ComponentApiError('invalid_response')
  if (!Array.isArray(row.provides)) throw new ComponentApiError('invalid_response')
  const view: ComponentView = {
    id: row.id,
    version: row.version,
    state: row.state,
    provides: row.provides.map(parseProvide),
  }
  if (row.form !== undefined) {
    if (typeof row.form !== 'string' || !(FORMS as readonly string[]).includes(row.form)) {
      throw new ComponentApiError('invalid_response')
    }
    view.form = row.form
  }
  if (row.reason !== undefined) {
    if (typeof row.reason !== 'string') throw new ComponentApiError('invalid_response')
    view.reason = row.reason
  }
  if (row.kernel_grants !== undefined) view.kernel_grants = parseStringList(row.kernel_grants)
  if (row.declared !== undefined) {
    const declared = object(row.declared)
    view.declared = {
      kernel: parseStringList(declared.kernel),
      operations: parseStringList(declared.operations),
    }
  }
  return view
}

export function parseComponentList(value: unknown): ComponentView[] {
  const row = object(value)
  if (!Array.isArray(row.components)) throw new ComponentApiError('invalid_response')
  return row.components.map(parseComponentView)
}

/** GET /api/v1/components. Throws ComponentApiError; never calls the report route. */
export async function listComponents(): Promise<ComponentView[]> {
  let response
  try {
    response = await window.agent24.backendProxy({ method: 'GET', path: '/api/v1/components' })
  } catch {
    throw new ComponentApiError('backend_not_ready')
  }
  if (!response.ok) throw new ComponentApiError('request_failed')
  return parseComponentList(response.data)
}
