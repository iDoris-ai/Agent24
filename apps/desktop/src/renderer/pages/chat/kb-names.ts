// L7-K1: names for the libraries a citation comes from. The server's hit
// carries only the library's id (READONLY R3.8: `display` is title and file
// name), and no host call lists names for the chat, so the user may give a
// name per bound library in the Knowledge settings. It is a display label kept
// on this machine, never sent, and never a way to reach a library: only a
// library the settings bind is labelled; any other id gets the plain 「知识库」.

const KEY = 'a24.knowledge.libraryNames'
const MAX = 40
// eslint-disable-next-line no-control-regex -- strips control and bidi characters from a label on purpose
const STRIP = /[\u0000-\u001f\u007f-\u009f؜‎‏‪-‮⁦-⁩]/gu

export type KbNames = Record<string, string>

/** 「id=名称」 or 「id 名称」 per line. */
export function parseKbNames(text: string): KbNames {
  const out: KbNames = {}
  for (const line of text.split('\n')) {
    // 「id=名称」 or 「id 名称」: an id has neither 「=」 nor a blank in it, so
    // whichever of the two comes first ends it (FU-219); a name may hold 「=」,
    // and 「id = 名称」 drops the one 「=」 right after the id.
    const text = line.trim()
    const eq = text.indexOf('=')
    const blank = text.search(/\s/)
    const at = eq < 0 ? blank : blank < 0 ? eq : Math.min(eq, blank)
    if (at < 0) continue
    const id = text.slice(0, at)
    const rest = text.slice(at).replace(/^\s*=?/, '')
    const name = [...rest.replace(STRIP, '').replace(/\s+/g, ' ').trim()].slice(0, MAX).join('')
    if (id !== '' && name !== '') out[id] = name
  }
  return out
}

export function formatKbNames(names: KbNames): string {
  return Object.entries(names).map(([id, name]) => `${id}=${name}`).join('\n')
}

export function loadKbNames(): KbNames {
  try { return parseKbNames(globalThis.localStorage?.getItem(KEY) ?? '') } catch { return {} }
}

export function saveKbNames(names: KbNames): void {
  try { globalThis.localStorage?.setItem(KEY, formatKbNames(names)) } catch { /* not kept; the labels fall back */ }
}

/** `numbered` has `{n}`; `plain` is for an id the settings do not bind. */
export function libraryLabel(id: string, bound: readonly string[], names: KbNames, numbered: string, plain = '知识库'): string {
  const at = bound.indexOf(id)
  if (at < 0) return plain
  // Own keys only: an id like 「constructor」 must not read Object.prototype (FU-219).
  return (Object.hasOwn(names, id) ? names[id] : undefined) ?? numbered.replace('{n}', String(at + 1))
}
