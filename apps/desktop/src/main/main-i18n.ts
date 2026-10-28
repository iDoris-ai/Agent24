// Main-process UI language for native dialogs.
//
// TODO(main merge): this is a deliberately tiny mirror of `main-i18n.ts` from
// origin/main (#1436), which this branch's base predates. It exposes the same
// surface (`mt`, `setMainLocale`, `getMainLocale`, `isMainLocale`,
// `resetMainLocaleForTest`) so the later rebase is: take main's file and add
// the `presence.*` keys below to its three tables.
//
// Default is English. The system locale is deliberately NOT consulted.

export type MainLocale = 'en' | 'zh' | 'th'
export const MAIN_DEFAULT_LOCALE: MainLocale = 'en'

export function isMainLocale(v: unknown): v is MainLocale {
  return v === 'en' || v === 'zh' || v === 'th'
}

const en = {
  'presence.title': 'Confirm on this computer',
  'presence.message': '{requester} (pid {pid}) is asking for permission: {operation}',
  'presence.detail': '{summary}\n\nOnly allow this if you started it yourself.',
  'presence.allow': 'Allow',
  'presence.deny': 'Deny',
} as const

export type MainMessageKey = keyof typeof en

const zh: Record<MainMessageKey, string> = {
  'presence.title': '请在本机确认',
  'presence.message': '{requester}（pid {pid}）请求授权：{operation}',
  'presence.detail': '{summary}\n\n只有确定是你自己发起的操作，才选择允许。',
  'presence.allow': '允许',
  'presence.deny': '拒绝',
}

const th: Record<MainMessageKey, string> = {
  'presence.title': 'ยืนยันบนเครื่องนี้',
  'presence.message': '{requester} (pid {pid}) ขออนุญาต: {operation}',
  'presence.detail': '{summary}\n\nอนุญาตเฉพาะเมื่อคุณเป็นผู้เริ่มการดำเนินการนี้เอง',
  'presence.allow': 'อนุญาต',
  'presence.deny': 'ปฏิเสธ',
}

export const MAIN_MESSAGE_TABLE: Record<MainLocale, Record<MainMessageKey, string>> = { en, zh, th }

let current: MainLocale = MAIN_DEFAULT_LOCALE

export function getMainLocale(): MainLocale {
  return current
}

/** Translate for the current language. */
export function mt(key: MainMessageKey, params: Record<string, string | number> = {}): string {
  return MAIN_MESSAGE_TABLE[current][key].replace(/\{(\w+)\}/g, (m, name: string) =>
    name in params ? String(params[name]) : m,
  )
}

/** Switch language. Unknown values are ignored. */
export function setMainLocale(locale: unknown): void {
  if (isMainLocale(locale)) current = locale
}

/** Test seam: back to the English default. */
export function resetMainLocaleForTest(): void {
  current = MAIN_DEFAULT_LOCALE
}
