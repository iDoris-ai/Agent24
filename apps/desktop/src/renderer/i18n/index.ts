// Desktop UI language: English (default), 中文, ไทย.
//
// One global, persisted setting — every page reads it through `useLocale` /
// `useT`, so switching it in Settings re-renders the whole app at once. No
// provider: the store is module-level (useSyncExternalStore), which keeps any
// page renderable on its own in tests.
//
// Copy lives next to the page that uses it (`<page>.i18n.ts`) and is declared
// with `defineMessages`, whose type makes zh / th carry exactly the keys en
// has — a missing translation is a compile error, not a blank label.

import { useSyncExternalStore } from 'react'

import { DEFAULT_LOCALE, isLocale, translate as lookup, type Locale, type Messages, type Vars } from '../../shared/i18n'
export { DEFAULT_LOCALE, isLocale, defineMessages, format } from '../../shared/i18n'
export type { Locale, Messages, Vars } from '../../shared/i18n'

export const LOCALES: readonly { id: Locale; label: string }[] = [
  { id: 'en', label: 'English' },
  { id: 'zh', label: '中文' },
  { id: 'th', label: 'ไทย' },
]

export const LOCALE_STORAGE_KEY = 'agent24:locale'

const HTML_LANG: Record<Locale, string> = { en: 'en', zh: 'zh-CN', th: 'th' }

function readStored(): Locale {
  try {
    const v = globalThis.localStorage?.getItem(LOCALE_STORAGE_KEY)
    if (isLocale(v)) return v
  } catch { /* storage unavailable — fall through to the default */ }
  return DEFAULT_LOCALE
}

function syncMainLocale(locale: Locale): void {
  void globalThis.window?.agent24?.setAppLocale?.(locale).catch(() => { /* main may be shutting down */ })
}

let current: Locale = readStored()
syncMainLocale(current)
const listeners = new Set<() => void>()

function applyHtmlLang(l: Locale) {
  if (typeof document !== 'undefined') document.documentElement.lang = HTML_LANG[l]
}
applyHtmlLang(current)

export function getLocale(): Locale {
  return current
}

/** Tell the main process which language the UI is in, so the tray and native
 * dialogs follow it. Best effort: absent outside Electron (tests). */
export function reportLocaleToMain(l: Locale = current): void {
  try {
    void (globalThis as { window?: { agent24?: { setUiLocale?: (l: string) => Promise<void> } } })
      .window?.agent24?.setUiLocale?.(l)?.catch(() => { /* main not listening */ })
  } catch { /* ignore */ }
}

export function setLocale(l: Locale): void {
  if (!isLocale(l)) return
  try { globalThis.localStorage?.setItem(LOCALE_STORAGE_KEY, l) } catch { /* not persisted; still switch */ }
  reportLocaleToMain(l)
  if (l === current) return
  current = l
  syncMainLocale(l)
  applyHtmlLang(l)
  listeners.forEach((fn) => fn())
}

function subscribe(fn: () => void): () => void {
  listeners.add(fn)
  return () => { listeners.delete(fn) }
}

export function useLocale(): Locale {
  return useSyncExternalStore(subscribe, getLocale, getLocale)
}

/** Non-hook lookup, for module-level helpers that are called during render. */
export function translate<K extends string>(m: Messages<K>, key: K, vars?: Vars, locale: Locale = current): string {
  return lookup(m, key, vars, locale)
}

export type T<K extends string> = (key: K, vars?: Vars) => string

export function useT<K extends string>(m: Messages<K>): T<K> {
  const locale = useLocale()
  return (key, vars) => translate(m, key, vars, locale)
}
