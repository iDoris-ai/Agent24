// L6-F5 (Loop 6, David 11-22): a rewrite or a filled template is saved as a
// new document unless the user says to save it into the original file; then
// it is the original's next version. Real-Mac lesson of F3/F2: a 4-bit model
// does not decide such things reliably, so the App judges the user's own
// words. Only a turn the App judged so carries SAVE_MARK in its prompt, with
// the instruction to pass save_to "original"; a card asking to save into the
// original on a run without the mark is sent back before anyone is asked
// (TaskCard). 「回到上一个版本」 never goes to the model: the App offers the
// version on a card and restores it when the user clicks.

import { localTime } from './delete-card'

/** Written into the prompt of a turn the App judged to save into the original. */
export const SAVE_MARK = '【App 判定：用户要求存到原文件】'

const ORIGINAL = '(?:原文件|原文档|原来的文件|原来的文档|原件)'
const SAVE_ASKED = [
  // 「存到原文件」「存回原文件」「直接存原文件」「保存到原文件」「就存在原来的文件里」
  new RegExp(`(?:存|保存|写)(?:回|到|进|在)?\\s*${ORIGINAL}`),
  // 「直接改原文件」「直接在原文件上改」
  new RegExp(`直接(?:改|修改|在|改动)\\s*${ORIGINAL}`),
  new RegExp(`覆盖\\s*${ORIGINAL}`),
  // 「存成这份文档的新版本」「保存为新版本」
  /(?:存|保存)(?:成|为)(?:这份文档的|这份文件的|原文件的|它的)?新(?:的)?版本/,
]
/** 「不要改原文件」「别存到原文件」「原文件不要动」. */
const SAVE_REFUSED = [
  new RegExp(`(?:不要|别|不用|不必|无需|勿|不)\\s*(?:直接)?\\s*(?:改|修改|存|保存|动|覆盖|写)[^，。,；;]{0,3}${ORIGINAL}`),
  new RegExp(`${ORIGINAL}[^，。,；;]{0,4}(?:不要|别|不能|不用)`),
]

/** Whether the user's words ask to save into the original file. */
export function asksSaveToOriginal(text: string): boolean {
  const said = text.replace(/《[^》]{1,200}》/g, '《》')
  return !SAVE_REFUSED.some((r) => r.test(said)) && SAVE_ASKED.some((r) => r.test(said))
}

/** Added to the prompt of a turn that asks it (doc-intent `docAgentPrompt`). */
export const SAVE_HINT = `${SAVE_MARK}（这次改写或填写的结果要存到原文件：调用 documents.patch 或 documents.fill 时带 save_to: "original"，结果会成为原文件的新版本，旧版本保留。）`

/** Whether a run's prompt is one the App judged to save into the original. */
export function savesToOriginal(prompt: string): boolean {
  return prompt.includes(SAVE_MARK)
}

const CN: Record<string, number> = { 一: 1, 二: 2, 两: 2, 三: 3, 四: 4, 五: 5, 六: 6, 七: 7, 八: 8, 九: 9, 十: 10 }
const RESTORE_VERB = '(?:回到|恢复到|恢复成|恢复|退回到|退回|还原到|还原成|还原|换回|改回|回退到|回退)'
const PREVIOUS = '(?:上一个|上一|上个|之前的|原来的|前一个)\\s*(?:版本|版)'
const NUMBERED = '第\\s*(\\d{1,3}|[一二两三四五六七八九十])\\s*(?:个)?\\s*(?:版本|版)'
const RESTORE = new RegExp(`${RESTORE_VERB}\\s*(?:${PREVIOUS}|${NUMBERED})`)
const NOT_RESTORE = new RegExp(`(?:不要|别|不用|先别|先不|不)\\s*${RESTORE_VERB}`)

export type RestoreAsk = { to: 'previous' | number }

/** Whether a turn asks to make an earlier version current again. */
export function asksRestoreVersion(text: string): RestoreAsk | null {
  if (NOT_RESTORE.test(text)) return null
  const m = RESTORE.exec(text)
  if (!m) return null
  if (m[1] === undefined) return { to: 'previous' }
  const n = /^\d+$/.test(m[1]) ? Number(m[1]) : CN[m[1]] ?? 0
  return n > 0 ? { to: n } : null
}

export interface VersionList {
  document_id: string
  head_revision: number
  revisions: Array<{ revision: number; created_at: string }>
}

export type RestoreOffer =
  | { kind: 'offer'; from: number; head: number; at: string }
  | { kind: 'none'; text: string }

/** The version a restore turn means, or why there is none. */
export function restoreOffer(versions: VersionList, ask: RestoreAsk): RestoreOffer {
  const head = versions.head_revision
  const from = ask.to === 'previous' ? head - 1 : ask.to
  if (head <= 1) return { kind: 'none', text: '这份文档只有一个版本，没有可以回到的上一个版本。' }
  if (from === head) return { kind: 'none', text: `第 ${head} 版就是现在的版本，不用恢复。` }
  const found = versions.revisions.find((r) => r.revision === from)
  if (!found) return { kind: 'none', text: `这份文档没有第 ${from} 版，现有第 1～${head} 版。` }
  return { kind: 'offer', from, head, at: localTime(found.created_at) }
}

/** The text above the restore card (kept in the history without the button). */
export function offerText(title: string, offer: { from: number; head: number; at: string }): string {
  return `《${title}》现在是第 ${offer.head} 版。要把第 ${offer.from} 版（保存于 ${offer.at}）恢复为当前版本吗？`
    + `恢复会把第 ${offer.from} 版的内容另存为第 ${offer.head + 1} 版，所有版本都保留。`
}

type Api = Window['agent24']['documents']

/** A restore the card offers: which document, which version, over which head. */
export interface RestoreCardData { documentId: string; title: string; from: number; head: number }

/** The restore card for a turn, or the text to answer instead. */
export async function restoreCardOf(api: Api, doc: { id: string; title: string }, ask: RestoreAsk): Promise<{ text: string; card?: RestoreCardData }> {
  const listed = await api.request({ op: 'revisions', documentId: doc.id }).catch(() => null)
  if (!listed || !listed.ok) return { text: `没能读取《${doc.title}》的版本，这次没有恢复。` }
  const offer = restoreOffer(listed.data, ask)
  if (offer.kind === 'none') return { text: offer.text }
  return { text: offerText(doc.title, offer), card: { documentId: doc.id, title: doc.title, from: offer.from, head: offer.head } }
}

/** What restoring said: the new version, or why nothing was written. */
export async function restoreVersion(api: Api, card: RestoreCardData): Promise<string> {
  const res = await api.request({ op: 'restoreRevision', documentId: card.documentId, revision: card.from, expectRevision: card.head }).catch(() => null)
  if (!res) return '没能恢复：连接文档服务失败，文档没有改动。'
  if (!res.ok) {
    return res.error.code === 'revision_conflict'
      ? `《${card.title}》在这期间有了新的版本，这次没有恢复。请再说一次「回到上一个版本」。`
      : `没能恢复，文档没有改动（${res.error.code}）。`
  }
  return res.data.committed
    ? `已恢复：第 ${card.from} 版的内容现在是《${res.data.title}》的第 ${res.data.revision} 版（当前版本），之前的版本都还在。`
    : `第 ${card.from} 版和现在的内容一样，没有新增版本。`
}
