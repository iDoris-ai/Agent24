import { describe, expect, it } from 'vitest'
import { isChatModelId } from './chatModels'

describe('isChatModelId', () => {
  it('keeps a chat model id', () => {
    expect(isChatModelId('Qwen3-8B-4bit')).toBe(true)
    expect(isChatModelId('Qwen3-30B')).toBe(true)
  })

  it('drops non-chat families by case-insensitive substring', () => {
    for (const id of [
      'FLUX.1-schnell',
      'GLM-OCR-bf16',
      'f5-tts',
      'VoiceClone',
      'CosyVoice2-0.5B',
      'mlx-whisper-large-v3',
      'paraformer-asr',
      'bge-m3-embed',
      'bge-m3',
      'ModernBERT-reranker',
      'Mage-VL',
      'stable-diffusion-xl',
      'sd-xl-turbo',
    ]) {
      expect(isChatModelId(id), id).toBe(false)
    }
  })
})
