import { useT } from '../i18n'
import { privacyMessages } from './privacy.i18n'

// VI v0.1 §4.2 (docs/brand/VI.md): where a model call ran, as a word plus a
// shape (● local, ↗ remote), never colour alone. Any tier other than
// local/remote (unknown, or a future value) renders nothing: no guessing.
const GLYPH = { local: '●', remote: '↗' } as const

export function PrivacyBadge({ tier }: { tier: string | null | undefined }) {
  const t = useT(privacyMessages)
  if (tier !== 'local' && tier !== 'remote') return null
  return (
    <span className={`privacy-badge privacy-badge-${tier}`} title={t(`${tier}.title`)}>
      <span aria-hidden="true">{GLYPH[tier]}</span>
      <span>{t(tier)}</span>
    </span>
  )
}
