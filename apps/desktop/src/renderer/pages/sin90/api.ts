// Read-only Sin90 calls. Writes do not go through this proxy.

import {
  readAreas, readBlocks, readDirections, readProposals, readReviews, readRhythms, readRoutines, readTasks, readToday, readWeekAttention, readWeeks,
  type Sin90Area, type Sin90Block, type Sin90Direction, type Sin90Proposal, type Sin90Review, type Sin90Rhythm, type Sin90Routine, type Sin90Task, type Sin90Today, type Sin90Week, type Sin90WeekAttention,
} from './model'

export class WriteFailed extends Error {
  constructor(message: string, readonly code: string) {
    super(message)
    this.name = 'WriteFailed'
  }
}

function errorOf(data: unknown): { code: string; message: string } {
  if (typeof data === 'object' && data !== null && 'error' in data) {
    const error = (data as { error?: { code?: unknown; message?: unknown } }).error
    return {
      code: typeof error?.code === 'string' ? error.code : '',
      message: typeof error?.message === 'string' ? error.message : '',
    }
  }
  return { code: '', message: '' }
}

function errorMessage(data: unknown, fallback: string): string {
  return errorOf(data).message || fallback
}

export async function sin90Get(path: string): Promise<unknown> {
  const res = await window.agent24.backendProxy({ method: 'GET', path })
  if (!res.ok) throw new Error(errorMessage(res.data, `GET ${path} failed`))
  return res.data
}

export function fetchAreas(): Promise<Sin90Area[]> {
  return sin90Get('/api/v1/sin90/areas').then(readAreas)
}

export function fetchDirections(): Promise<Sin90Direction[]> {
  return sin90Get('/api/v1/sin90/directions').then(readDirections)
}

export function fetchTasks(directionId: string): Promise<Sin90Task[]> {
  return sin90Get(`/api/v1/sin90/tasks?direction_id=${encodeURIComponent(directionId)}`).then(readTasks)
}

export function fetchToday(): Promise<Sin90Today> {
  return sin90Get('/api/v1/sin90/today').then(readToday)
}

export function fetchWeeks(): Promise<Sin90Week[]> {
  return sin90Get('/api/v1/sin90/weeks').then(readWeeks)
}

export function fetchBlocks(): Promise<Sin90Block[]> {
  return sin90Get('/api/v1/sin90/schedule-blocks').then(readBlocks)
}

export function fetchRoutines(): Promise<Sin90Routine[]> {
  return sin90Get('/api/v1/sin90/routines').then(readRoutines)
}

export function fetchRhythms(): Promise<Sin90Rhythm[]> {
  return sin90Get('/api/v1/sin90/rhythms').then(readRhythms)
}

export function fetchProposals(): Promise<Sin90Proposal[]> {
  return sin90Get('/api/v1/sin90/proposals').then(readProposals)
}

export function fetchReviews(): Promise<Sin90Review[]> {
  return sin90Get('/api/v1/sin90/reviews').then(readReviews)
}

export function fetchWeekAttention(weekId: string): Promise<Sin90WeekAttention> {
  return sin90Get(`/api/v1/sin90/weeks/${encodeURIComponent(weekId)}/attention`).then(readWeekAttention)
}

export async function writesAvailable(): Promise<boolean> {
  const ask = window.agent24.sin90KeyStatus
  if (!ask) return false
  try {
    return (await ask()).available === true
  } catch {
    return false
  }
}

export async function sin90Write(body: Record<string, unknown>): Promise<void> {
  const write = window.agent24.sin90Write
  if (!write) throw new Error('sin90 write is unavailable')
  const res = await write(body)
  if (!res.ok) {
    const { code, message } = errorOf(res.data)
    throw new WriteFailed(message || 'sin90 write failed', code)
  }
}
