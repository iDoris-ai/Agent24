import { randomBytes } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import {
  CHAT_GROUP_ID,
  CHAT_GROUPS_FORMAT,
  CHAT_GROUPS_VERSION,
  CHAT_HISTORY_FORMAT,
  CHAT_HISTORY_ID,
  CHAT_HISTORY_INITIATORS,
  CHAT_HISTORY_PENDING_KINDS,
  CHAT_HISTORY_RUN_KINDS,
  CHAT_HISTORY_TITLE_SOURCES,
  CHAT_HISTORY_VERSION,
  type ChatHistoryInitiator,
  type ChatHistoryLatency,
  type ChatHistoryMessage,
  type ChatHistoryPending,
  type ChatHistoryRecord,
  type ChatHistoryRun,
  type ChatHistoryRunKind,
  type ChatHistorySummary,
  type ChatHistoryTitleSource,
  type ChatGroup,
  type ChatGroupsState,
  chatHistorySummary,
} from '../shared/chat-history'

/** One conversation's file may not exceed this, so a runaway page cannot
 *  fill the disk through one IPC call. */
const MAX_RECORD_BYTES = 8 * 1024 * 1024
/** Fewer bytes than the fixed part of any written message, its strings
 *  left empty: `{"role":"user","content":"","at":"","delivered":true}` is
 *  52; the strings are counted separately below. */
const MIN_MESSAGE_BYTES = 48
/** A title set by `setTitle` is at most this many UTF-16 units. The page
 *  keeps a rename to 80 characters; one character (an emoji sequence, a
 *  flag) can take a dozen units or more, so this leaves room for them. */
const MAX_TITLE_UNITS = 4000
/** M4 A2: how many runs and waiting items one conversation keeps, and how
 *  long a waiting item's label may be (UTF-16 units). */
const MAX_RUNS = 1000
const MAX_PENDING = 100
const MAX_LABEL_UNITS = 1000
/** A run's or a waiting item's id: the daemon's ids are short and plain. */
const ITEM_ID = /^[A-Za-z0-9_-]{1,128}$/
const isItemId = (v: unknown): v is string => typeof v === 'string' && ITEM_ID.test(v)

/** M4 A2: told of every change written through appendRun, promoteRun,
 *  setPending, clearPending or markSeen (M6 C1) (main's own modules, and
 *  the page's promoteRun and markSeen), with the summary as it now stands, so the open window can
 *  show it (main.ts forwards it). Not of saves: the page made those. */
type ChangeListener = (summary: ChatHistorySummary) => void
let changeListener: ChangeListener | null = null
export function onChatHistoryChange(listener: ChangeListener | null): void {
  changeListener = listener
}

/** A title replaces the one on file only when its source is at least as
 *  strong: the user's rename over main's own name (a schedule's, M4 A5, or
 *  the assistant's conversation, A6) over the model's title over the first
 *  prompt. */
const TITLE_RANK: Record<ChatHistoryTitleSource, number> = { prompt: 0, model: 1, schedule: 2, assistant: 2, user: 3 }
const isTitleSource = (v: unknown): v is ChatHistoryTitleSource =>
  typeof v === 'string' && (CHAT_HISTORY_TITLE_SOURCES as readonly string[]).includes(v)
const isInitiator = (v: unknown): v is ChatHistoryInitiator =>
  typeof v === 'string' && (CHAT_HISTORY_INITIATORS as readonly string[]).includes(v)

/** A cheap lower bound of the written size, checked before the full parse,
 *  so an oversized save is refused without first parsing and serializing
 *  all of it on the main thread. Never more than the real size (a UTF-16
 *  unit is at least one UTF-8 byte), so a record that fits is never refused. */
function precheck(v: unknown): void {
  if (!isObject(v)) return
  const strings = (...xs: unknown[]) => xs.reduce<number>((n, x) => n + (typeof x === 'string' ? x.length : 0), 0)
  let bytes = strings(v.id, v.title, v.created_at, v.updated_at)
  const tooLarge = () => { if (bytes > MAX_RECORD_BYTES) throw new Error('Chat history record too large') }
  tooLarge()
  if (!Array.isArray(v.messages)) return
  for (const m of v.messages) {
    bytes += MIN_MESSAGE_BYTES
    if (isObject(m)) {
      bytes += strings(m.content, m.at, m.run_id)
      if (isObject(m.latency)) bytes += strings(m.latency.modelName, m.latency.tier)
    }
    tooLarge()
  }
  // Not the page's runs and waiting items: a save never writes them (M4 A2),
  // and their counts and sizes are capped by the parse; `write` checks the
  // real size of what is written (Codex A2 C3).
}

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v)
/** ISO 8601 in UTC, as `Date.prototype.toISOString` writes it: no local
 *  times, so the order and the instant survive a move to another zone. */
const UTC_TIME = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,3})?Z$/
const isTime = (v: unknown): v is string => typeof v === 'string' && UTC_TIME.test(v) && !Number.isNaN(Date.parse(v))
const isNullableString = (v: unknown): v is string | null => v === null || typeof v === 'string'
const isNullableNumber = (v: unknown): v is number | null => v === null || (typeof v === 'number' && Number.isFinite(v))

function invalid(): never {
  throw new Error('Invalid chat history record')
}

function parseLatency(v: unknown): ChatHistoryLatency | null {
  if (v === null) return null
  if (!isObject(v) || !isNullableString(v.modelName) || !isNullableString(v.tier)
    || !isNullableNumber(v.firstTokenMs) || typeof v.totalMs !== 'number' || !Number.isFinite(v.totalMs)) invalid()
  return { modelName: v.modelName, tier: v.tier, firstTokenMs: v.firstTokenMs, totalMs: v.totalMs }
}

function parseMessage(v: unknown): ChatHistoryMessage {
  if (!isObject(v) || typeof v.content !== 'string' || !isTime(v.at)) invalid()
  if (v.role === 'user') {
    if (typeof v.delivered !== 'boolean') invalid()
    return { role: 'user', content: v.content, at: v.at, delivered: v.delivered }
  }
  if (v.role === 'assistant') {
    if (!isNullableString(v.run_id)) invalid()
    return { role: 'assistant', content: v.content, at: v.at, run_id: v.run_id, latency: parseLatency(v.latency) }
  }
  invalid()
}

const isRunKind = (v: unknown): v is ChatHistoryRunKind =>
  typeof v === 'string' && (CHAT_HISTORY_RUN_KINDS as readonly string[]).includes(v)

function parseRun(v: unknown): ChatHistoryRun {
  if (!isObject(v) || !isItemId(v.run_id) || !isRunKind(v.kind) || !isTime(v.started_at)) invalid()
  return { run_id: v.run_id, kind: v.kind, started_at: v.started_at }
}

/** A waiting item, or null when it is not one. */
function pendingOf(v: unknown): ChatHistoryPending | null {
  if (!isObject(v) || !isItemId(v.id) || typeof v.kind !== 'string'
    || !(CHAT_HISTORY_PENDING_KINDS as readonly string[]).includes(v.kind)
    || (v.run_id !== undefined && !isItemId(v.run_id))
    || typeof v.label !== 'string' || !v.label.trim() || v.label.length > MAX_LABEL_UNITS || !isTime(v.at)) return null
  const item: ChatHistoryPending = { id: v.id, kind: v.kind as ChatHistoryPending['kind'], label: v.label, at: v.at }
  if (v.run_id !== undefined) item.run_id = v.run_id
  return item
}

function parsePending(v: unknown): ChatHistoryPending {
  return pendingOf(v) ?? invalid()
}

/** Strict: unknown versions (versions 1 and 2 among them: there is nothing
 *  of them to keep) and malformed fields are refused, never guessed at.
 *  M6 C1: `initiator` is one of CHAT_HISTORY_INITIATORS; `unseen` is an
 *  array (no holes) of run ids, each one of `runs`, none twice, so never
 *  more than MAX_RUNS; a conversation the user started has none. */
export function parseChatHistoryRecord(v: unknown): ChatHistoryRecord {
  if (!isObject(v) || v.format !== CHAT_HISTORY_FORMAT || v.version !== CHAT_HISTORY_VERSION
    || typeof v.id !== 'string' || !CHAT_HISTORY_ID.test(v.id) || typeof v.title !== 'string'
    || !isTitleSource(v.title_source)
    || (v.group_id !== null && !(typeof v.group_id === 'string' && CHAT_GROUP_ID.test(v.group_id)))
    || (v.archived_at !== null && !isTime(v.archived_at))
    || !isTime(v.created_at) || !isTime(v.updated_at) || !Array.isArray(v.messages)
    || !Array.isArray(v.runs) || v.runs.length > MAX_RUNS
    || !Array.isArray(v.pending) || v.pending.length > MAX_PENDING) invalid()
  // Array.from visits holes as undefined, which the parsers refuse: a sparse
  // array would otherwise be written with nulls and spoil the file.
  const runs = Array.from(v.runs as unknown[], parseRun)
  const pending = Array.from(v.pending as unknown[], parsePending)
  if (new Set(runs.map(r => r.run_id)).size !== runs.length
    || new Set(pending.map(p => p.id)).size !== pending.length) invalid()
  // M6 C1: who started it, and the unseen runs: each one of its runs, none
  // twice, none when the user started it.
  if (!isInitiator(v.initiator) || !Array.isArray(v.unseen)) invalid()
  const runIds = new Set(runs.map(r => r.run_id))
  const unseen = Array.from(v.unseen, (item): string => {
    if (!isItemId(item) || !runIds.has(item)) invalid()
    return item
  })
  if (new Set(unseen).size !== unseen.length || (v.initiator === 'user' && unseen.length > 0)) invalid()
  return {
    format: CHAT_HISTORY_FORMAT,
    version: CHAT_HISTORY_VERSION,
    id: v.id,
    title: v.title,
    title_source: v.title_source,
    group_id: v.group_id as string | null,
    archived_at: v.archived_at as string | null,
    created_at: v.created_at,
    updated_at: v.updated_at,
    messages: Array.from(v.messages as unknown[], parseMessage),
    runs,
    pending,
    initiator: v.initiator,
    unseen,
  }
}

/** M4 A2: the runs on file, and each reply's run the file does not know yet
 *  as an answer, when the reply came. Past the cap, a new answer is left
 *  out: answers are never shown, tasks are. */
function withAnswerRuns(runs: readonly ChatHistoryRun[], messages: readonly ChatHistoryMessage[]): ChatHistoryRun[] {
  const out = [...runs]
  const known = new Set(out.map(r => r.run_id))
  for (const m of messages) {
    if (m.role !== 'assistant' || m.run_id === null || known.has(m.run_id) || !ITEM_ID.test(m.run_id)) continue
    if (out.length >= MAX_RUNS) break
    known.add(m.run_id)
    out.push({ run_id: m.run_id, kind: 'answer', started_at: m.at })
  }
  return out
}

const summaryOf = chatHistorySummary

/** UI-06: how many groups the list keeps, and how long a group's name may
 *  be (UTF-16 units; the page keeps names far shorter). */
const MAX_GROUPS = 200
const MAX_GROUP_NAME_UNITS = 1000
const GROUPS_DEFAULT: ChatGroupsState = { groups: [], assistant_collapsed: false, archive_collapsed: true }

/** Strict like the conversations: a damaged or foreign file reads as no groups. */
export function parseChatGroups(v: unknown): ChatGroupsState {
  if (!isObject(v) || v.format !== CHAT_GROUPS_FORMAT || v.version !== CHAT_GROUPS_VERSION
    || !Array.isArray(v.groups) || v.groups.length > MAX_GROUPS
    || typeof v.assistant_collapsed !== 'boolean' || typeof v.archive_collapsed !== 'boolean') invalid()
  const seen = new Set<string>()
  const groups = Array.from(v.groups as unknown[], (g): ChatGroup => {
    if (!isObject(g) || typeof g.id !== 'string' || !CHAT_GROUP_ID.test(g.id) || seen.has(g.id)
      || typeof g.name !== 'string' || !g.name || g.name.length > MAX_GROUP_NAME_UNITS
      || !isTime(g.created_at) || typeof g.collapsed !== 'boolean') invalid()
    seen.add(g.id)
    return { id: g.id, name: g.name, created_at: g.created_at, collapsed: g.collapsed }
  })
  return { groups, assistant_collapsed: v.assistant_collapsed, archive_collapsed: v.archive_collapsed }
}

const groupName = (name: unknown): string => {
  const text = typeof name === 'string' ? name.replace(/\s+/g, ' ').trim() : ''
  if (!text || text.length > MAX_GROUP_NAME_UNITS) throw new Error('Invalid chat group name')
  return text
}

/** The main chat's history: one JSON file per conversation under
 *  `<dir>/chat-history/`, written by atomic replacement like nav-prefs.
 *  A damaged or foreign file is skipped by `list` and reads as missing. */
export function createChatHistoryStore(dir: string) {
  const root = path.join(dir, 'chat-history')
  const checkId = (id: unknown): string => {
    if (typeof id !== 'string' || !CHAT_HISTORY_ID.test(id)) throw new Error('Invalid chat history id')
    return id
  }
  const fileOf = (id: unknown): string => path.join(root, `${checkId(id)}.json`)
  /** A save's temporary file: `<id>.json.<random>.tmp`. One left by a save
   *  that died before its rename still holds the conversation, so delete and
   *  clear remove these too. */
  const tempOf = (id: string): string => path.join(root, `${id}.json.${randomBytes(6).toString('hex')}.tmp`)
  const tempOwner = (name: string): string | null => {
    const m = /^(chat_[a-z0-9_]{1,80})\.json\.[0-9a-f]{12}\.tmp$/.exec(name)
    return m ? m[1] : null
  }
  const names = (): string[] => {
    try { return fs.readdirSync(root) }
    catch { return [] }
  }
  /** A conversation's file, or null when it is missing, damaged or not
   *  this version. For a change (`forChange`), a file that is there but cannot
   *  be read (a permission, an I/O error) refuses the change rather than
   *  lose what main keeps in it (Codex A2 C1), as the groups do. */
  const read = (file: string, forChange = false): ChatHistoryRecord | null => {
    let text: string
    try { text = fs.readFileSync(file, 'utf8') }
    catch (e) {
      if (forChange && (e as NodeJS.ErrnoException).code !== 'ENOENT') throw new Error('Chat history unreadable')
      return null
    }
    try { return parseChatHistoryRecord(JSON.parse(text)) }
    catch { return null }
  }
  /** The conversation on file under its own id, or null. */
  const current = (id: string, forChange = false): ChatHistoryRecord | null => {
    const record = read(fileOf(id), forChange)
    return record && record.id === id ? record : null
  }
  /** Writes a file of this store by atomic replacement. */
  const replace = (file: string, temp: string, body: string): void => {
    fs.mkdirSync(root, { recursive: true, mode: 0o700 })
    // A fresh name, created exclusively: never follows or reuses a file
    // already there, and is ours to remove.
    const fd = fs.openSync(temp, 'wx', 0o600)
    let renamed = false
    try {
      try { fs.writeFileSync(fd, body) }
      finally { fs.closeSync(fd) }
      fs.renameSync(temp, file)
      renamed = true
    } finally {
      // Ours from the moment it was opened: a failed or partial write
      // (e.g. a full disk) leaves no piece of the transcript behind.
      if (!renamed) fs.rmSync(temp, { force: true })
    }
  }
  const write = (record: ChatHistoryRecord): void => {
    const body = JSON.stringify(record)
    if (Buffer.byteLength(body) > MAX_RECORD_BYTES) throw new Error('Chat history record too large')
    replace(fileOf(record.id), tempOf(record.id), body)
  }
  /** Reads, changes and writes one conversation's runs or waiting items:
   *  `edit` answers the record to write, or null when nothing changes. A
   *  written change is told to the change listener. */
  const change = (id: string, edit: (record: ChatHistoryRecord) => ChatHistoryRecord | null): ChatHistorySummary | null => {
    const before = current(id, true)
    if (!before) return null
    const after = edit(before)
    if (!after) return summaryOf(before)
    write(after)
    const summary = summaryOf(after)
    changeListener?.(summary)
    return summary
  }
  // UI-06: the groups file. Its name is not a conversation's, so list and
  // clear leave it alone: clearing the history keeps the groups.
  const groupsFile = path.join(root, '_groups.json')
  /** Where a groups file that could not be read is moved before a change
   *  writes a new one: the user's groups are set aside, not overwritten,
   *  each time under a name of its own. */
  const groupsAside = (): string => path.join(root, `_groups.damaged.${Date.now()}.${randomBytes(4).toString('hex')}.json`)
  const groupsTemp = /^_groups\.json\.[0-9a-f]{12}\.tmp$/
  const noGroups = (): ChatGroupsState => ({ ...GROUPS_DEFAULT, groups: [] })
  /** The groups on file. A missing file is no groups. For showing them, a
   *  damaged one is no groups too; for changing them (`forChange`), it is
   *  moved aside first, and a file that is there but cannot be read (a
   *  permission, an I/O error) refuses the change rather than lose it. */
  const readGroups = (forChange = false): ChatGroupsState => {
    let text: string
    try { text = fs.readFileSync(groupsFile, 'utf8') }
    catch (e) {
      if ((e as NodeJS.ErrnoException).code === 'ENOENT' || !forChange) return noGroups()
      throw new Error('Chat groups unreadable')
    }
    try { return parseChatGroups(JSON.parse(text)) }
    catch {
      if (forChange) fs.renameSync(groupsFile, groupsAside())
      return noGroups()
    }
  }
  const writeGroups = (state: ChatGroupsState): ChatGroupsState => {
    const body = JSON.stringify({ format: CHAT_GROUPS_FORMAT, version: CHAT_GROUPS_VERSION, ...state })
    replace(groupsFile, path.join(root, `_groups.json.${randomBytes(6).toString('hex')}.tmp`), body)
    return state
  }
  /** Temporary files of group writes that died before their rename: they
   *  may hold the names of groups deleted since. */
  const sweepGroupTemps = (): void => {
    for (const name of names()) if (groupsTemp.test(name)) fs.rmSync(path.join(root, name), { force: true })
  }
  const checkGroup = (id: unknown): string => {
    if (typeof id !== 'string' || !CHAT_GROUP_ID.test(id)) throw new Error('Invalid chat group id')
    return id
  }
  /** `asked`: the user asked the run in this conversation (promoteRun, the
   *  page's): never unseen, also one the conversation did not know yet. */
  const recordRun = (id: unknown, runId: unknown, kind: unknown, asked: boolean): ChatHistorySummary | null => {
    const own = checkId(id)
    if (!isItemId(runId)) throw new Error('Invalid chat history run')
    if (!isRunKind(kind)) throw new Error('Invalid chat history run kind')
    return change(own, record => {
      const known = record.runs.find(r => r.run_id === runId)
      if (known) {
        if (known.kind !== 'answer' || kind === 'answer') return null
        return { ...record, runs: record.runs.map(r => (r.run_id === runId ? { ...r, kind } : r)) }
      }
      let runs = record.runs
      if (runs.length >= MAX_RUNS) {
        // Full: the oldest answer makes room (answers are never shown,
        // tasks are; PR-Daemon #1696 L1). Only tasks and schedules' runs
        // left, or an answer to add: refused.
        const oldest = runs.findIndex(r => r.kind === 'answer')
        if (oldest < 0 || kind === 'answer') throw new Error('Too many chat history runs')
        runs = runs.filter((_, i) => i !== oldest)
      }
      // M6 C1: a new run, other than an answer or one the user asked, of a
      // conversation the user did not start is unseen; one that made room
      // above leaves `unseen`.
      const kept = new Set(runs.map(r => r.run_id))
      const unseen = record.unseen.filter(id => kept.has(id))
      if (record.initiator !== 'user' && kind !== 'answer' && !asked) unseen.push(runId)
      return { ...record, runs: [...runs, { run_id: runId, kind, started_at: new Date().toISOString() }], unseen }
    })
  }
  const appendRun = (id: unknown, runId: unknown, kind: unknown): ChatHistorySummary | null => recordRun(id, runId, kind, false)
  return {
    /** Newest first. */
    list(): ChatHistorySummary[] {
      return names()
        .filter(name => name.endsWith('.json') && CHAT_HISTORY_ID.test(name.slice(0, -5)))
        .flatMap(name => {
          const record = read(path.join(root, name))
          return record && `${record.id}.json` === name ? [summaryOf(record)] : []
        })
        .sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at))
    },
    get(id: unknown): ChatHistoryRecord | null {
      return current(checkId(id))
    },
    /** M4 A5: a new conversation main starts itself (a schedule's, or the
     *  assistant's, A6), with no messages yet; M6 C1: who started it, never
     *  'user' (the page's conversations start with a save). Told to the
     *  change listener, so the open window lists it. docs/ui/ASSISTANT-A.md,
     *  ASSISTANT-C.md §2. */
    create(input: { title: string; title_source: ChatHistoryTitleSource; initiator: ChatHistoryInitiator }): ChatHistorySummary {
      // Main's conversations are a schedule's or the assistant's, never the user's.
      if (typeof input?.title !== 'string' || input.title.length > MAX_TITLE_UNITS || !isTitleSource(input.title_source)
        || !isInitiator(input.initiator) || input.initiator === 'user') {
        throw new Error('Invalid chat history conversation')
      }
      const at = new Date().toISOString()
      const record: ChatHistoryRecord = {
        format: CHAT_HISTORY_FORMAT, version: CHAT_HISTORY_VERSION,
        id: `chat_${Date.now().toString(36)}_${randomBytes(6).toString('hex')}`,
        title: input.title, title_source: input.title_source, group_id: null, archived_at: null,
        created_at: at, updated_at: at, messages: [], runs: [], pending: [],
        initiator: input.initiator, unseen: [],
      }
      write(record)
      const summary = summaryOf(record)
      changeListener?.(summary)
      return summary
    },
    /** Writes the conversation and returns its summary as written. The page
     *  sends the first prompt's title; a stronger title already on file (a
     *  rename, the model's title) is kept, so neither a later save nor a
     *  late retry of an older one undoes it. */
    save(value: unknown): ChatHistorySummary {
      precheck(value)
      let record = parseChatHistoryRecord(value)
      const before = current(record.id, true)
      // A save never retitles at its own rank either (PR-Daemon #1640 L3):
      // titles change through setTitle, so a late retry of an older save
      // carrying a model or user title cannot undo a newer one. Only the
      // first prompt's title, recomputed on every save, follows the save.
      const keep = before && (TITLE_RANK[before.title_source] > TITLE_RANK[record.title_source]
        || (before.title_source === record.title_source && record.title_source !== 'prompt'))
      if (before && keep) {
        record = { ...record, title: before.title, title_source: before.title_source }
      }
      // Its group and whether it is archived are the user's, changed only
      // through organize (UI-06): a save, or a late retry of an older one,
      // keeps what is on file and never sets them.
      record = { ...record, group_id: before?.group_id ?? null, archived_at: before?.archived_at ?? null }
      // Its runs and what waits on the user are main's too (M4 A2): the save
      // keeps those on file and adds only the runs of the replies it carries.
      record = { ...record, runs: withAnswerRuns(before?.runs ?? [], record.messages), pending: before?.pending ?? [] }
      // Who started it and what of it is unseen are main's too (M6 C1): kept
      // from the file; one not on file yet is the user's, with nothing unseen.
      record = { ...record, initiator: before?.initiator ?? 'user', unseen: before?.unseen ?? [] }
      write(record)
      return summaryOf(record)
    },
    // M4 A2: runs and waiting items, for main's own modules (and promoteRun
    // for the page). Each changes a conversation already on file, never
    // creates one, leaves its messages and `updated_at` alone, and returns
    // its summary as it now stands, or null when there is no such
    // conversation. docs/ui/ASSISTANT-A.md.
    /** Records a run the conversation started. A run it knows already
     *  keeps its kind and time, except that an answer becomes the kind given. */
    appendRun,
    /** An answer run becomes a task (`promotionDue`, or the user handed it
     *  over); recorded first when the conversation did not know it yet. A
     *  task or a schedule's run stays what it is. Only the page calls it,
     *  for a run the user asked in this conversation: never unseen (M6 C1,
     *  Codex C1 R1), whoever started the conversation. */
    promoteRun(id: unknown, runId: unknown): ChatHistorySummary | null {
      return recordRun(id, runId, 'task', true)
    },
    /** Adds what waits on the user, or replaces the one with the same id. */
    setPending(id: unknown, item: unknown): ChatHistorySummary | null {
      const own = checkId(id)
      const next = pendingOf(item)
      if (!next) throw new Error('Invalid chat history pending item')
      return change(own, record => {
        const at = record.pending.findIndex(p => p.id === next.id)
        if (at < 0 && record.pending.length >= MAX_PENDING) throw new Error('Too many chat history pending items')
        const pending = at < 0 ? [...record.pending, next] : record.pending.map((p, i) => (i === at ? next : p))
        return { ...record, pending }
      })
    },
    /** M6 C1: the user opened the conversation (or has it open): nothing of
     *  it is unseen any more. Its runs stay, so a catch-up that routes one
     *  of them again does not make it unseen again. Nothing is written (and
     *  nothing told) when nothing was unseen. docs/ui/ASSISTANT-C.md §4. */
    markSeen(id: unknown): ChatHistorySummary | null {
      const own = checkId(id)
      return change(own, r => (r.unseen.length > 0 ? { ...r, unseen: [] } : null))
    },
    /** Takes it off; nothing to do when it is not there. */
    clearPending(id: unknown, pendingId: unknown): ChatHistorySummary | null {
      const own = checkId(id)
      if (!isItemId(pendingId)) throw new Error('Invalid chat history pending item')
      return change(own, record => (record.pending.some(p => p.id === pendingId)
        ? { ...record, pending: record.pending.filter(p => p.id !== pendingId) }
        : null))
    },
    /** UI-06: moves a conversation already on file into a group (null: out
     *  of any) and archives or restores it. Returns its summary as it now
     *  stands, or null when there is no such conversation (never creates one).
     *  Leaves the messages and `updated_at` alone: archiving or moving does
     *  not reorder the list. A group must exist; archiving keeps the group, so
     *  a restored conversation goes back where it was. */
    organize(id: unknown, patch: unknown): ChatHistorySummary | null {
      const own = checkId(id)
      if (!isObject(patch) || Object.keys(patch).some(k => k !== 'group_id' && k !== 'archived')
        || (patch.group_id === undefined && patch.archived === undefined)
        || (patch.archived !== undefined && typeof patch.archived !== 'boolean')) {
        throw new Error('Invalid chat history change')
      }
      if (patch.group_id !== undefined && patch.group_id !== null) {
        const group = checkGroup(patch.group_id)
        if (!readGroups().groups.some(g => g.id === group)) throw new Error('Unknown chat group')
      }
      const before = current(own)
      if (!before) return null
      const record: ChatHistoryRecord = { ...before }
      if (patch.group_id !== undefined) record.group_id = patch.group_id as string | null
      if (patch.archived === true) record.archived_at = before.archived_at ?? new Date().toISOString()
      if (patch.archived === false) record.archived_at = null
      write(record)
      return summaryOf(record)
    },
    groups(): ChatGroupsState {
      return readGroups()
    },
    /** Adds a group at the end; returns the groups as they now stand. */
    createGroup(name: unknown): ChatGroupsState {
      const text = groupName(name)
      const state = readGroups(true)
      if (state.groups.length >= MAX_GROUPS) throw new Error('Too many chat groups')
      const group: ChatGroup = {
        id: `grp_${Date.now().toString(36)}_${randomBytes(4).toString('hex')}`,
        name: text, created_at: new Date().toISOString(), collapsed: false,
      }
      return writeGroups({ ...state, groups: [...state.groups, group] })
    },
    renameGroup(id: unknown, name: unknown): ChatGroupsState {
      const own = checkGroup(id)
      const text = groupName(name)
      const state = readGroups(true)
      if (!state.groups.some(g => g.id === own)) return state
      return writeGroups({ ...state, groups: state.groups.map(g => (g.id === own ? { ...g, name: text } : g)) })
    },
    /** Removes a group. Its conversations are not touched: one whose group
     *  no longer exists is listed with the others, so the deletion is one
     *  write that cannot stop halfway. */
    deleteGroup(id: unknown): ChatGroupsState {
      const own = checkGroup(id)
      const state = readGroups(true)
      // Gone already (maybe a deletion that stopped before its sweep): the
      // sweep still runs.
      if (!state.groups.some(g => g.id === own)) { sweepGroupTemps(); return state }
      const after = writeGroups({ ...state, groups: state.groups.filter(g => g.id !== own) })
      sweepGroupTemps()
      return after
    },
    /** Folds or unfolds a group, the whole list ('assistant') or the
     *  archived conversations ('archive'). */
    setCollapsed(target: unknown, collapsed: unknown): ChatGroupsState {
      if (typeof collapsed !== 'boolean') throw new Error('Invalid chat history fold')
      // Checked before the file is read for a change (PR-Daemon #1647 L1).
      const own = target === 'assistant' || target === 'archive' ? null : checkGroup(target)
      const state = readGroups(true)
      if (target === 'assistant') return writeGroups({ ...state, assistant_collapsed: collapsed })
      if (target === 'archive') return writeGroups({ ...state, archive_collapsed: collapsed })
      if (own === null || !state.groups.some(g => g.id === own)) return state
      return writeGroups({ ...state, groups: state.groups.map(g => (g.id === own ? { ...g, collapsed } : g)) })
    },
    /** Retitles a conversation already on file: `user` for a rename, `model`
     *  for a generated title, which never replaces a rename. Returns the
     *  summary as it now stands, or null when there is no such conversation
     *  (never creates one: it may have just been deleted). Leaves the
     *  messages and `updated_at` alone, so a rename does not reorder the list. */
    setTitle(id: unknown, title: unknown, source: unknown): ChatHistorySummary | null {
      const own = checkId(id)
      if (source !== 'user' && source !== 'model') throw new Error('Invalid chat history title source')
      const text = typeof title === 'string' ? title.replace(/\s+/g, ' ').trim() : ''
      if (!text || text.length > MAX_TITLE_UNITS) throw new Error('Invalid chat history title')
      const before = current(own)
      if (!before) return null
      if (TITLE_RANK[before.title_source] > TITLE_RANK[source]) return summaryOf(before)
      const record: ChatHistoryRecord = { ...before, title: text, title_source: source }
      write(record)
      return summaryOf(record)
    },
    delete(id: unknown): void {
      const own = checkId(id)
      fs.rmSync(fileOf(own), { force: true })
      for (const name of names()) if (tempOwner(name) === own) fs.rmSync(path.join(root, name), { force: true })
    },
    /** Removes every conversation this store wrote, including temporary files
     *  of interrupted saves; leaves anything else alone. */
    clear(): void {
      for (const name of names()) {
        const conversation = name.endsWith('.json') && CHAT_HISTORY_ID.test(name.slice(0, -5))
        if (conversation || tempOwner(name) !== null) fs.rmSync(path.join(root, name), { force: true })
      }
      sweepGroupTemps()
    },
  }
}
