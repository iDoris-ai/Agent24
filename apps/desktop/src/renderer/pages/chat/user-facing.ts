// M3 (Loop 5): what the user reads carries no internal names. A 4-bit model
// writes 「通过 `documents.get("合同.pdf")` 确认」 whatever the prompt says, and a
// stopped turn read 「回答失败：run_cancelled」. The prompt asks the model not to
// (docAgentPrompt, KNOWLEDGE_HINT); this is the floor under it.

/** The tools a model is offered, as the user would say them. */
const TOOL_WORDS: Record<string, string> = {
  'documents.list': '文档列表',
  'documents.get': '查看文档',
  'documents.compose': '生成新文档',
  'documents.paragraphs': '查看段落',
  'documents.patch': '改写段落',
  'documents.fields': '查看模板字段',
  'documents.fill': '填写模板',
  knowledge_search: '检索知识库',
  knowledge_read: '读取知识库原文',
  fs_read: '读取文件',
  fs_write: '写入文件',
  shell_exec: '执行命令',
  http_fetch: '访问网页',
  self_wake: '定时提醒',
  propose_plan: '提交计划',
}

/** The documents operations, `knowledge_x` and the built-in names with `_`,
 * with an optional call `(…)` after and backticks around. A file named
 * documents.pdf and plain English words (`explore`) are left alone. */
const NAME = /`?\b(documents\.(?:list|get|compose|paragraphs|patch|fields|fill|read_range|find|extract)\b|knowledge_[a-z_]+|fs_read|fs_write|shell_exec|http_fetch|self_wake|propose_plan)\b(\([^()\n]{0,200}\))?`?/g

/** The answer with every internal tool name said in the user's words. */
export function withoutToolNames(text: string): string {
  return text.replace(NAME, (_all, name: string) => TOOL_WORDS[name] ?? (name.startsWith('documents.') ? '文档操作' : name.startsWith('knowledge_') ? '知识库操作' : name))
}

export type FailureKind = 'cancelled' | 'timeout' | 'notStarted' | 'model' | 'other'

/** A run's error code as one of the few things the user is told. */
export function failureKind(code: string): FailureKind {
  if (code === 'run_cancelled' || code === 'cancelled') return 'cancelled'
  if (code === 'run_timeout' || code === 'timeout') return 'timeout'
  if (code === 'run_not_started' || code === 'backend_not_ready' || code === 'session_unavailable') return 'notStarted'
  if (/provider|idoris|model/.test(code)) return 'model'
  return 'other'
}
