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

export type Locale = 'en' | 'zh' | 'th'

export const LOCALES: readonly { id: Locale; label: string }[] = [
  { id: 'en', label: 'English' },
  { id: 'zh', label: '中文' },
  { id: 'th', label: 'ไทย' },
]

export const DEFAULT_LOCALE: Locale = 'en'
export const LOCALE_STORAGE_KEY = 'agent24:locale'

const HTML_LANG: Record<Locale, string> = { en: 'en', zh: 'zh-CN', th: 'th' }

export function isLocale(v: unknown): v is Locale {
  return v === 'en' || v === 'zh' || v === 'th'
}

function readStored(): Locale {
  try {
    const v = globalThis.localStorage?.getItem(LOCALE_STORAGE_KEY)
    if (isLocale(v)) return v
  } catch { /* storage unavailable — fall through to the default */ }
  return DEFAULT_LOCALE
}

let current: Locale = readStored()
const listeners = new Set<() => void>()

function applyHtmlLang(l: Locale) {
  if (typeof document !== 'undefined') document.documentElement.lang = HTML_LANG[l]
}
applyHtmlLang(current)

export function getLocale(): Locale {
  return current
}

export function setLocale(l: Locale): void {
  if (!isLocale(l)) return
  try { globalThis.localStorage?.setItem(LOCALE_STORAGE_KEY, l) } catch { /* not persisted; still switch */ }
  if (l === current) return
  current = l
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

export interface Messages<K extends string> {
  en: Record<K, string>
  zh: Record<K, string>
  th: Record<K, string>
}

/** Declare a page's copy. en defines the keys; zh and th must match them exactly. */
export function defineMessages<K extends string>(m: {
  en: Record<K, string>
  zh: { [P in K]: string }
  th: { [P in K]: string }
}): Messages<K> {
  return m
}

export type Vars = Record<string, string | number>

/** `{name}` placeholders; unknown placeholders are left as-is. */
export function format(s: string, vars?: Vars): string {
  if (!vars) return s
  return s.replace(/\{(\w+)\}/g, (m, k: string) => (k in vars ? String(vars[k]) : m))
}

/** Non-hook lookup, for module-level helpers that are called during render. */
export function translate<K extends string>(m: Messages<K>, key: K, vars?: Vars, locale: Locale = current): string {
  return format(m[locale][key] ?? m.en[key] ?? key, vars)
}

export type T<K extends string> = (key: K, vars?: Vars) => string

export function useT<K extends string>(m: Messages<K>): T<K> {
  const locale = useLocale()
  return (key, vars) => translate(m, key, vars, locale)
}
