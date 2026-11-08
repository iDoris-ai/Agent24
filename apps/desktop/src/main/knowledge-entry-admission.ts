// KB-SOP-01a (#1669): may this conversation become a knowledge entry candidate
// for an organisation knowledge base? Host duty under CONTRACT-OPS-v2 W8.1.
//
// Rule (David 2026-11-09, via General): the user picked the conversation, the
// user confirms explicitly, and its classification is Ordinary. Restricted,
// Unknown or missing are refused, in plain words, before anything leaves this
// machine, and there is no "continue anyway".
//
// Classification (General 2026-11-09):
// - The conversation's own session floor covers every reply without a run id:
//   /api/v1/chat replies merge into the floor of the chat's session_id, which
//   is the history record's id; a failed Knowledge turn only stores local text.
// - Every reply with a run id adds that run's classification.
// - The strictest one wins. A run that cannot be found or read, or Unknown
//   anywhere, refuses.
// - The conversation's own floor must exist only when some reply has no run
//   id (General 2026-11-09, option ②, #1669): a conversation answered wholly
//   by Knowledge runs never touched /api/v1/chat, so its floor is missing by
//   design and its runs say all. A failed Knowledge turn stores local text
//   without a run id, so it still needs the floor: fail closed.

import type { ChatHistoryRecord } from '../shared/chat-history'

export type Classification = 'ordinary' | 'restricted' | 'unknown'
/** What the daemon reports for a session floor: `missing` when it has no row. */
export type FloorReading = Classification | 'missing'

/** At most this many runs are asked about; a longer conversation is refused, never cut short. */
export const MAX_ADMISSION_RUNS = 64

export interface AdmissionReader {
  /**
   * The strictest classification of the session floor of `sessionId` and of
   * each run in `runIds`. Read only. Throws (or rejects) when it cannot tell,
   * which counts as Unknown.
   */
  read(sessionId: string, runIds: readonly string[]): Promise<FloorReading>
}

export type AdmissionRefusal = 'floor_missing' | 'restricted' | 'unknown' | 'too_many_runs' | 'not_confirmed'
export type Admission = { ok: true } | { ok: false; reason: AdmissionRefusal }

/** The distinct run ids of the conversation's replies, in order. */
export function replyRuns(record: Pick<ChatHistoryRecord, 'messages'>): string[] {
  const seen = new Set<string>()
  for (const message of record.messages) {
    if (message.role === 'assistant' && message.run_id !== null) seen.add(message.run_id)
  }
  return [...seen]
}

/** Whether some reply has no run id, i.e. came from /api/v1/chat or was a failed Knowledge turn. */
function hasReplyWithoutRun(record: Pick<ChatHistoryRecord, 'messages'>): boolean {
  return record.messages.some((message) => message.role === 'assistant' && message.run_id === null)
}

const RANK: Record<FloorReading, number> = { ordinary: 0, missing: 1, unknown: 2, restricted: 3 }

/** The strictest of `readings`; an empty list is Unknown, never Ordinary. */
export function strictest(readings: readonly FloorReading[]): FloorReading {
  if (readings.length === 0) return 'unknown'
  return readings.reduce((worst, next) => (RANK[next] > RANK[worst] ? next : worst))
}

function refuse(reading: FloorReading): Admission {
  if (reading === 'ordinary') return { ok: true }
  if (reading === 'missing') return { ok: false, reason: 'floor_missing' }
  return { ok: false, reason: reading }
}

/**
 * Classification only: whether the conversation may be offered at all. The
 * caller still needs the user's explicit confirmation, see {@link admit}.
 */
export async function classify(record: Pick<ChatHistoryRecord, 'id' | 'messages'>, reader: AdmissionReader): Promise<Admission> {
  const runs = replyRuns(record)
  if (runs.length > MAX_ADMISSION_RUNS) return { ok: false, reason: 'too_many_runs' }
  let reading: FloorReading
  try {
    reading = await reader.read(record.id, runs)
  } catch {
    return { ok: false, reason: 'unknown' }
  }
  if (!Object.hasOwn(RANK, reading)) return { ok: false, reason: 'unknown' }
  // `missing` is only ever the answer when every listed run is Ordinary.
  if (reading === 'missing' && runs.length > 0 && !hasReplyWithoutRun(record)) return { ok: true }
  return refuse(reading)
}

/**
 * The full admission: classification first (so a refusal can be said before
 * asking anything), then the user's confirmation. `confirmed` must be true only
 * when the user confirmed this conversation in a host dialog just now.
 */
export async function admit(record: Pick<ChatHistoryRecord, 'id' | 'messages'>, reader: AdmissionReader, confirmed: boolean): Promise<Admission> {
  const classified = await classify(record, reader)
  if (!classified.ok) return classified
  return confirmed === true ? { ok: true } : { ok: false, reason: 'not_confirmed' }
}
