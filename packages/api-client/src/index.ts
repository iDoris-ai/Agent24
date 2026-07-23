// Hand-written barrel — the ONLY non-generated file in src/.
// Everything it re-exports comes from `pnpm gen:api` output; CI fails on drift.

// REST: OpenAPI paths/components (protocol/openapi.yaml)
export type { paths, components, operations } from './openapi'

// WS events (protocol/events.schema.json — GENERATED from agent24-protocol
// since B4; payload-level types replace the old per-event wrapper types)
export type {
  Agent24V1WebSocketEventProtocol as V1Event,
  RunStartedPayload,
  ModelDeltaPayload,
  RunCompletedPayload,
  RunOutputPayload,
  RunFailedPayload,
  RunCancelledPayload,
  ToolStartedPayload,
  ToolCompletedPayload,
  ToolCompletedStatus,
  ApprovalResolvedPayload,
  ScheduleFiredPayload,
  ScheduleDisabledPayload,
  Approval,
  ApprovalStatus,
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
