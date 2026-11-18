// Read-only Sin90 calls. Writes do not go through this proxy.

import { readAreas, readDirections, readTasks, readToday, type Sin90Area, type Sin90Direction, type Sin90Task, type Sin90Today } from './model'

function errorMessage(data: unknown, fallback: string): string {
  if (typeof data === 'object' && data !== null && 'error' in data) {
    const message = (data as { error?: { message?: unknown } }).error?.message
    if (typeof message === 'string' && message) return message
  }
  return fallback
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
