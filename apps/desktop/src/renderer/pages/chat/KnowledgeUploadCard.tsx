// L7-K2: files picked in the chat. Main opens the dialog and keeps the path;
// this card only shows the id, the name and the state.

import { useEffect, useState } from 'react'
import type { KnowledgeUpload } from '../../../shared/ipc-types'
import { uploadLine } from './knowledge-onboard'

function canCancel(u: KnowledgeUpload): boolean {
  return u.state === 'saved' || u.state === 'processing' || (u.state === 'indexed' && u.enrichment === 'running')
}

export function KnowledgeUploadCard({ uploadIds, onPicked }: { uploadIds: string[]; onPicked: (ids: string[]) => void }) {
  const [uploads, setUploads] = useState<KnowledgeUpload[]>([])

  useEffect(() => {
    const api = window.agent24?.knowledge
    if (!api?.uploadList || !api.onUploadChanged) return
    let alive = true
    let live = false
    const stop = api.onUploadChanged((list) => {
      live = true
      if (alive) setUploads(list)
    })
    void api.uploadList().then((list) => {
      if (alive && !live) setUploads(list)
    }, () => { /* main is gone; the list stays empty */ })
    return () => { alive = false; stop() }
  }, [])

  const pick = async (): Promise<void> => {
    const api = window.agent24?.knowledge
    if (!api?.uploadPick) return
    let picked: KnowledgeUpload[]
    try {
      picked = await api.uploadPick()
    } catch {
      return
    }
    if (picked.length === 0) return
    setUploads((prev) => {
      const byId = new Map(prev.map((u) => [u.uploadId, u]))
      for (const u of picked) byId.set(u.uploadId, u)
      return [...byId.values()]
    })
    onPicked(picked.map((u) => u.uploadId))
  }

  const cancel = (id: string): void => {
    const run = window.agent24?.knowledge?.uploadCancel
    if (!run) return
    void run(id).catch(() => { /* the row stays until the next list */ })
  }

  const shown = uploads.filter((u) => uploadIds.includes(u.uploadId))
  return (
    <div role="group" aria-label="上传文档" className="extract-card">
      <button type="button" className="btn btn-primary" onClick={() => { void pick() }}>上传文档</button>
      <p className="extract-card-muted">支持 .md、.txt，单个不超过 10 MB；PDF、Word 暂不支持，选了也不会上传。</p>
      {shown.map((u) => (
        <div key={u.uploadId} className="extract-card-value">
          <span>{uploadLine(u)}</span>
          {canCancel(u) && (
            <button type="button" className="btn btn-ghost" onClick={() => cancel(u.uploadId)}>取消</button>
          )}
        </div>
      ))}
    </div>
  )
}
