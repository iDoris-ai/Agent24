// Chat-model name heuristic. oMLX's model list has no kind, so Settings and
// the topbar share this filter. iDoris `/v1/models/local` `kind` replaces it.
//
// `mage` is a substring, so it also matches `image`.

const NON_CHAT_PARTS = [
  'flux',
  'ocr',
  'tts',
  'voice',
  'cosyvoice',
  'whisper',
  'asr',
  'embed',
  'bge',
  'rerank',
  'mage',
  'diffusion',
  'sd-',
]

// Families the topbar already skipped that the substring list does not name
// (video, a few VL spellings). Kept here so there is one rule set.
const EXTRA_NON_CHAT: RegExp[] = [
  /\bsdxl\b/i,
  /\bsd3\b/i,
  /kolors/i,
  /pixart/i,
  /\bltx\b/i,
  /cogvideo/i,
  /hunyuan-?video/i,
  /\bwan-?2/i,
  /\bvits\b/i,
  /\bbark\b/i,
  /\bgte\b/i,
  /\be5\b/i,
  /-vl\b/i,
  /\bvl-/i,
]

export function isChatModelId(id: string): boolean {
  const name = id.toLowerCase()
  if (NON_CHAT_PARTS.some((part) => name.includes(part))) return false
  return !EXTRA_NON_CHAT.some((re) => re.test(id))
}
