// M6 C2: "The assistant has something to say (n)" on top of the main chat
// (docs/ui/ASSISTANT-C.md §7). Shown only while n > 0; a press opens the
// conversation whose earliest unseen run came first (`bannerTarget`).

import { useSyncExternalStore } from 'react'
import { useT } from '../../i18n'
import { chatMessages } from '../Chat.i18n'
import { Banner } from '../../components/Banner'
import { getHistoryState, openConversation, subscribeHistory } from './history'
import { bannerCount, bannerNumber, bannerTarget } from './proactive'

export function ProactiveBanner() {
  const { items, busy } = useSyncExternalStore(subscribeHistory, getHistoryState)
  const t = useT(chatMessages)
  const n = bannerCount(items)
  if (n === 0) return null
  return <Banner text={t('proactive.banner', { n: bannerNumber(n) })}
    label={t('proactive.bannerLabel', { n: String(n) })}
    disabled={busy} onActivate={() => { const id = bannerTarget(items); if (id) openConversation(id) }} />
}
