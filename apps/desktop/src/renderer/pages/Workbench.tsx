import { useT } from '../i18n'
import { workbenchMessages } from './Workbench.i18n'

// AUDIT-3: none of these cards has an onClick handler and the "翻译" card
// used to be labelled "✓ 可用" (implying it's clickable and working), which
// is misleading — clicking it did nothing (no navigation, no request). Every
// card is now uniformly a disabled roadmap preview: "即将推出", greyed out,
// `cursor: not-allowed`, `aria-disabled`.
const CAPABILITIES = [
  { icon: '🎙️', key: 'asr', tag: 'mlx-whisper' },
  { icon: '🔊', key: 'tts', tag: 'Kokoro / CosyVoice2' },
  { icon: '🖼️', key: 't2i', tag: 'mflux' },
  { icon: '🎬', key: 'video', tag: 'LTX-2.3 Q4' },
  { icon: '📄', key: 'ocr', tag: 'GOT-OCR2' },
  { icon: '🌐', key: 'translate', tag: 'Qwen3-30B' },
  { icon: '🕷️', key: 'crawl', tag: 'Crawl4AI' },
  { icon: '🚀', key: 'publish', tag: 'Playwright' },
  { icon: '🔍', key: 'rag', tag: 'ChromaDB' },
] as const

export default function WorkbenchPage() {
  const t = useT(workbenchMessages)
  return (
    <div className="content">
      <div className="page-title">{t('title')}</div>
      <div className="page-sub">{t('sub')}</div>
      <div className="workbench-grid">
        {CAPABILITIES.map(cap => (
          <div key={cap.key} className="capability-card" aria-disabled="true">
            <div className="cap-icon">{cap.icon}</div>
            <h3>{t(`cap.${cap.key}.name`)}</h3>
            <p>{t(`cap.${cap.key}.desc`)}</p>
            <div className="cap-status">
              <span className="tag">{cap.tag}</span>
              {' '}
              {t('comingSoon')}
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}
