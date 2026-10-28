export type Locale = 'en' | 'zh' | 'th'
export const DEFAULT_LOCALE: Locale = 'en'
export function isLocale(v: unknown): v is Locale {
  return v === 'en' || v === 'zh' || v === 'th'
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

export function translate<K extends string>(m: Messages<K>, key: K, vars?: Vars, locale: Locale = DEFAULT_LOCALE): string {
  return format(m[locale][key] ?? m.en[key] ?? key, vars)
}
