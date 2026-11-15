import type { ComponentIsolation } from './componentApi'
import { useT } from '../i18n'
import { isolationMessages } from './isolation.i18n'

export function IsolationTag({ isolation }: { isolation: ComponentIsolation | undefined }) {
  const t = useT(isolationMessages)
  if (!isolation) return null
  const text = isolation.state === 'isolated'
    ? t('isolated')
    : isolation.reason === 'platform_unsupported'
      ? t('platform')
      : t('form')
  return <span className="tag cap-isolation">{text}</span>
}
