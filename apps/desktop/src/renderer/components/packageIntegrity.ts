/** Fixed host prompts for an OS package integrity refusal (S1-9 §4). */
const HINTS: Record<string, string> = {
  integrity_manifest_missing: '包版本过旧，升级 Sin90/Cos72 后重试',
  hash_mismatch: '文件校验失败，请重装可信版本',
  integrity_manifest_invalid: '安装内容不完整或格式不合法，请重装',
  signature_rejected: '发布签名验证失败，请安装可信版本',
  integrity_check_failed: '无法校验包，请检查访问权限后重试',
}

export function packageIntegrityHint(reason: string | null | undefined): string | undefined {
  if (!reason) return undefined
  return HINTS[reason]
}
