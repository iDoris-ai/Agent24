// L6-F2: what the delete card says of the one document it removes: its title,
// pages (a PDF's) and import time, read from the library by its id, and how
// many other documents share its title. Never the model's words.

type Api = Window['agent24']['documents'] | undefined
type T = (key: string, vars?: Record<string, string>) => string

/** A title as the documents module compares them: no case, quotes or extension. */
const plain = (raw: string): string => raw.trim().replace(/^[《「"“]+|[》」"”]+$/g, '').trim().toLowerCase().replace(/\.[a-z0-9]{1,5}$/, '')

export async function deleteTarget(api: Api, id: string, t: T): Promise<string> {
  const res = api ? await api.request({ op: 'get', documentId: id }).catch(() => null) : null
  if (!res || !res.ok) return t('task.ask.deleteUnread')
  const doc = res.data as { title: string; created_at: string; pages?: unknown }
  const pages = typeof doc.pages === 'number' ? t('task.ask.deletePages', { n: String(doc.pages) }) : t('task.ask.deletePagesUnknown')
  const at = new Date(doc.created_at).toLocaleString()
  const list = await api!.request({ op: 'list', limit: 200 }).catch(() => null)
  const same = list && list.ok ? list.data.documents.filter((d) => d.document_id !== id && plain(d.title) === plain(doc.title)).length : 0
  return t('task.ask.deleteTarget', { title: doc.title, pages, at }) + (same > 0 ? t('task.ask.deleteSame', { n: String(same) }) : '')
}
