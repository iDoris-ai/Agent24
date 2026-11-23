// L6-F2: what the delete card says of the one document it removes: its title,
// pages (a PDF's) and import time, read from the library by its id, and how
// many other documents share its title. Never the model's words.

type Api = Window['agent24']['documents'] | undefined
type T = (key: string, vars?: Record<string, string>) => string

/** A title as the documents module compares them (agent24-documents
 * `plain_title`, main's `plainTitle`): no case, quotes, space or extension. */
export function plainTitle(raw: string): string {
  const s = raw.trim().replace(/^[《》「」"“”]+|[《》「」"“”]+$/g, '').trim().toLowerCase()
  const dot = s.lastIndexOf('.')
  const ext = dot > 0 ? s.slice(dot + 1) : ''
  return dot > 0 && ext.length >= 1 && ext.length <= 5 && /^[a-z0-9]+$/.test(ext) ? s.slice(0, dot) : s
}

/** An import time as the native box writes it (main `localTime`): 2026-11-21 14:05. */
export function localTime(iso: unknown): string {
  const d = new Date(typeof iso === 'string' ? iso : '')
  if (Number.isNaN(d.getTime())) return typeof iso === 'string' ? iso : ''
  const two = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())} ${two(d.getHours())}:${two(d.getMinutes())}`
}

export async function deleteTarget(api: Api, id: string, t: T): Promise<string> {
  const res = api ? await api.request({ op: 'get', documentId: id }).catch(() => null) : null
  if (!res || !res.ok) return t('task.ask.deleteUnread')
  const doc = res.data as { title?: unknown; created_at?: unknown; pages?: unknown } | null
  if (typeof doc?.title !== 'string' || !doc.title) return t('task.ask.deleteUnread')
  const pages = typeof doc.pages === 'number' ? t('task.ask.deletePages', { n: String(doc.pages) }) : t('task.ask.deletePagesUnknown')
  const at = localTime(doc.created_at)
  const list = await api!.request({ op: 'list', limit: 200 }).catch(() => null)
  // PR-Daemon #2028 L1: not read, or not the whole library, is said so; it is
  // never taken for 「no other document has this title」.
  const whole = list && list.ok && !list.data.next_cursor
  const same = whole ? list.data.documents.filter((d) => d.document_id !== id && plainTitle(d.title) === plainTitle(doc.title as string)).length : 0
  const note = !whole ? t('task.ask.deleteSameUnknown') : same > 0 ? t('task.ask.deleteSame', { n: String(same) }) : ''
  return t('task.ask.deleteTarget', { title: doc.title as string, pages, at }) + note
}
