// KB-SOP-01a (#1669): "沉淀为知识条目" — the one action the history list
// gains (UI line frozen; positions checked by PM), and the panel it opens on
// the Knowledge page. The renderer only names the conversation: main reads
// its own copy, refuses it unless it is Ordinary and asks the user in a
// native dialog (main/knowledge-entry-ipc.ts). Drafting comes with KB-SOP-01b.

import { useContext, useEffect, useState, useSyncExternalStore } from 'react'
import type { CapabilityId } from '../../capabilities/types'
import type { KnowledgeEntryAdmission } from '../../../shared/ipc-types'
import { CapabilitiesContext } from '../../capabilities/types'
import { defineMessages, useT } from '../../i18n'
import type { MenuEntry } from './HistoryMenu'

export const knowledgeEntryMessages = defineMessages({
  en: {
    'menu.entry': 'Turn into knowledge entry',
    'panel.title': 'Knowledge entry',
    'panel.checking': 'Checking this conversation…',
    'panel.admitted': 'This conversation can become a knowledge entry. Drafting it here comes in a later update.',
    'refused.restricted': 'This conversation used restricted material, so it cannot become a knowledge entry.',
    'refused.unknown': 'Agent24 cannot tell what this conversation was drawn from, so it cannot become a knowledge entry.',
    'refused.floor_missing': 'Agent24 has no record of where this conversation’s replies came from, so it cannot become a knowledge entry.',
    'refused.too_many_runs': 'This conversation is too long to check. Start a shorter one for the knowledge entry.',
    'refused.not_confirmed': 'Nothing was done.',
    'refused.not_found': 'This conversation is no longer here.',
    'refused.busy': 'Another conversation is waiting for your answer. Answer it first.',
    'refused.knowledge_off': 'Knowledge is off or not bound to a knowledge base. Turn it on in Settings first.',
    'refused.failed': 'This conversation could not be checked. Try again in a moment.',
    'action.close': 'Close',
  },
  zh: {
    'menu.entry': '沉淀为知识条目',
    'panel.title': '知识条目',
    'panel.checking': '正在检查这条对话…',
    'panel.admitted': '这条对话可以整理成知识条目。在这里编辑草稿的功能稍后提供。',
    'refused.restricted': '这条对话用到了受限资料，不能整理成知识条目。',
    'refused.unknown': 'Agent24 无法确认这条对话用到了哪些资料，不能整理成知识条目。',
    'refused.floor_missing': 'Agent24 没有这条对话回复来源的记录，不能整理成知识条目。',
    'refused.too_many_runs': '这条对话太长，无法检查。请用一条较短的对话整理知识条目。',
    'refused.not_confirmed': '没有做任何操作。',
    'refused.not_found': '这条对话已经不在了。',
    'refused.busy': '另一条对话正在等你确认，请先处理那一条。',
    'refused.knowledge_off': 'Knowledge 未开启或未绑定知识库，请先在设置里开启。',
    'refused.failed': '暂时无法检查这条对话，请稍后再试。',
    'action.close': '关闭',
  },
  th: {
    'menu.entry': 'ทำเป็นรายการความรู้',
    'panel.title': 'รายการความรู้',
    'panel.checking': 'กำลังตรวจสอบบทสนทนานี้…',
    'panel.admitted': 'บทสนทนานี้ทำเป็นรายการความรู้ได้ การแก้ไขฉบับร่างที่นี่จะมาในการอัปเดตครั้งถัดไป',
    'refused.restricted': 'บทสนทนานี้ใช้ข้อมูลที่จำกัดการเข้าถึง จึงทำเป็นรายการความรู้ไม่ได้',
    'refused.unknown': 'Agent24 ไม่สามารถยืนยันได้ว่าบทสนทนานี้ใช้ข้อมูลจากที่ใด จึงทำเป็นรายการความรู้ไม่ได้',
    'refused.floor_missing': 'Agent24 ไม่มีบันทึกว่าคำตอบในบทสนทนานี้มาจากที่ใด จึงทำเป็นรายการความรู้ไม่ได้',
    'refused.too_many_runs': 'บทสนทนานี้ยาวเกินกว่าจะตรวจสอบได้ โปรดใช้บทสนทนาที่สั้นกว่านี้',
    'refused.not_confirmed': 'ไม่ได้ดำเนินการใด ๆ',
    'refused.not_found': 'ไม่พบบทสนทนานี้แล้ว',
    'refused.busy': 'มีอีกบทสนทนาหนึ่งรอการยืนยันจากคุณอยู่ โปรดตอบบทสนทนานั้นก่อน',
    'refused.knowledge_off': 'Knowledge ปิดอยู่หรือยังไม่ได้เชื่อมกับฐานความรู้ โปรดเปิดในการตั้งค่าก่อน',
    'refused.failed': 'ยังตรวจสอบบทสนทนานี้ไม่ได้ โปรดลองอีกครั้งในอีกสักครู่',
    'action.close': 'ปิด',
  },
})

/** The Knowledge page's capability id (capabilities/registry.ts). */
export const KNOWLEDGE_PAGE: CapabilityId = 'knowledgeBase'

/** What the Knowledge page shows about the conversation last asked about. */
export type EntryAsk =
  | { id: string; state: 'checking' }
  | { id: string; state: 'done'; answer: KnowledgeEntryAdmission }
  | { id: string; state: 'failed' }

// Module state, so the list can hand the question to the page without App or
// Chat carrying it. The question is asked once, from the click, never from a
// render or an effect (StrictMode runs those twice). One renderer has one main
// window, so one panel state is enough; a second window would share it.
let current: EntryAsk | null = null
let asked = 0
const listeners = new Set<() => void>()

function publish(next: EntryAsk | null): void {
  current = next
  for (const listener of listeners) listener()
}

/** Asks main about conversation `id`; null closes the panel. */
export function requestEntry(id: string | null): void {
  const token = ++asked
  if (id === null) {
    publish(null)
    return
  }
  publish({ id, state: 'checking' })
  let reply: Promise<unknown>
  try {
    reply = Promise.resolve(window.agent24.knowledgeEntry.admit(id))
  } catch (err) {
    reply = Promise.reject(err)
  }
  reply.then(
    (answer) => {
      if (token !== asked) return
      publish(isAdmission(answer) ? { id, state: 'done', answer } : { id, state: 'failed' })
    },
    () => { if (token === asked) publish({ id, state: 'failed' }) },
  )
}

/** Main's answer as the panel can show it; anything else reads as a failure. */
function isAdmission(value: unknown): value is KnowledgeEntryAdmission {
  if (typeof value !== 'object' || value === null) return false
  const v = value as { ok?: unknown; reason?: unknown }
  if (v.ok === true) return true
  return v.ok === false && typeof v.reason === 'string' && Object.hasOwn(knowledgeEntryMessages.en, `refused.${v.reason}`)
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => { listeners.delete(listener) }
}

export function usePendingEntry(): EntryAsk | null {
  return useSyncExternalStore(subscribe, () => current)
}

/**
 * Whether Knowledge is on and bound now. Main says so only on request, so it
 * is asked again on mount, when the window comes back, and on every press
 * that may open a history menu (an open menu takes the change in place).
 * Not known yet, or the question failed: off.
 */
function useKnowledgeBound(): boolean {
  const [bound, setBound] = useState(false)
  useEffect(() => {
    let live = true
    const refresh = (): void => {
      // Some pages and tests run without the preload bridge: that is off too.
      const status = window.agent24?.knowledge?.status?.()
      if (!status) { setBound(false); return }
      status.then((s) => { if (live) setBound(s.active === true) }, () => { if (live) setBound(false) })
    }
    refresh()
    window.addEventListener('focus', refresh)
    document.addEventListener('pointerdown', refresh, true)
    document.addEventListener('contextmenu', refresh, true)
    return () => {
      live = false
      window.removeEventListener('focus', refresh)
      document.removeEventListener('pointerdown', refresh, true)
      document.removeEventListener('contextmenu', refresh, true)
    }
  }, [])
  return bound
}

/** The history list's one new action, for any conversation, archived ones
 *  included; none at all while Knowledge is off or not bound. */
export function useEntryMenu(): (id: string) => MenuEntry[] {
  const t = useT(knowledgeEntryMessages)
  const { open } = useContext(CapabilitiesContext)
  const bound = useKnowledgeBound()
  return (id) => (!bound ? [] : [{
    kind: 'item',
    key: 'knowledge-entry',
    label: t('menu.entry'),
    // The panel and the native dialog take the focus.
    focuses: true,
    onSelect: () => {
      requestEntry(id)
      open(KNOWLEDGE_PAGE, 'other')
    },
  }])
}

/** The panel on the Knowledge page while a conversation is asked about. */
export function KnowledgeEntryPanel({ ask }: { ask: EntryAsk }): JSX.Element {
  const t = useT(knowledgeEntryMessages)
  const text = ask.state === 'checking'
    ? t('panel.checking')
    : ask.state === 'failed'
      ? t('refused.failed')
      : ask.answer.ok
        ? t('panel.admitted')
        : t(`refused.${ask.answer.reason}`)
  return (
    <section className="knowledge-confirm" role="dialog" aria-modal="false" aria-label={t('panel.title')}>
      <p role="status">{text}</p>
      {ask.state !== 'checking' && (
        <button className="btn" type="button" onClick={() => requestEntry(null)}>{t('action.close')}</button>
      )}
    </section>
  )
}
