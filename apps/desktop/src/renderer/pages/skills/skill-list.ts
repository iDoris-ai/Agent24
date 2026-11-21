// M2 (Loop 5): 「读取技能列表失败」 on the chat page and the capabilities page,
// while `agent24 skill list` answers. The pages ask once, when they mount; with
// the App-managed daemon that is before it has announced its port and
// credential, so main answers `backend_not_ready` (503) and the failure stays
// until the window is refocused. A daemon that is not there yet is waited for
// a while; any other answer is final.

type Proxy = Window['agent24']['backendProxy']
type Answer = Awaited<ReturnType<Proxy>>

/** Waits between tries: about 30 s in all, as long as the daemon may take to start. */
export const SKILL_LIST_RETRY_MS = [1_000, 2_000, 4_000, 8_000, 15_000] as const

/** Not there yet (main has no connection, or the daemon is not answering):
 * worth asking again. */
export function notYetThere(res: Answer | null): boolean {
  if (res === null) return true
  if (res.ok) return false
  const code = (res.data as { error?: { code?: unknown } } | null)?.error?.code
  return code === 'backend_not_ready' || code === 'proxy_failed' || code === 'timeout' || res.status === 503
}

/** `GET /api/v1/skills`, asked again while the daemon is not there yet. */
export async function getSkillList(
  proxy: Proxy,
  wait: (ms: number) => Promise<void> = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
): Promise<Answer> {
  let res: Answer | null = null
  for (let attempt = 0; ; attempt++) {
    res = await Promise.resolve(proxy({ method: 'GET', path: '/api/v1/skills' })).catch(() => null)
    if (!notYetThere(res) || attempt >= SKILL_LIST_RETRY_MS.length) break
    await wait(SKILL_LIST_RETRY_MS[attempt])
  }
  return res ?? { ok: false, status: 503, data: { error: { code: 'backend_not_ready', message: 'daemon not ready' } } }
}
