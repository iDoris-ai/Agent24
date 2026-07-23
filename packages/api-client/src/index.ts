// Hand-written barrel — the ONLY non-generated file in src/.
// Everything it re-exports comes from `pnpm gen:api` output; CI fails on drift.

// REST: OpenAPI paths/components (protocol/openapi.yaml)
export type { paths, components, operations } from './openapi'

// WS events (protocol/events.schema.json)
export type {
  Agent24V1WebSocketEventProtocol as V1Event,
  Envelope,
  RunStarted,
  RunCompleted,
  RunFailed,
  RunCancelled,
  ModelDelta,
  ToolStarted,
  ToolCompleted,
  ApprovalRequired,
  ApprovalResolved,
  ScheduleFired,
  ScheduleDisabled,
  Approval,
  Decision,
  Usage,
  ErrorBody,
} from './events'

// Convenience aliases for the most-used REST schemas
import type { components as C } from './openapi'
export type Run = C['schemas']['Run']
export type Session = C['schemas']['Session']
export type ToolCall = C['schemas']['ToolCall']
export type Schedule = C['schemas']['Schedule']
export type Model = C['schemas']['Model']
export type ApiError = C['schemas']['Error']
