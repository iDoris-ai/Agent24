import { describe, expect, it, vi } from 'vitest'
import { getSkillList, SKILL_LIST_RETRY_MS } from './skill-list'

const notReady = { ok: false, status: 503, data: { error: { code: 'backend_not_ready', message: 'x' } } }
const listed = { ok: true, status: 200, data: { skills: [] } }

describe('getSkillList (M2: 「读取技能列表失败」 while the App-managed daemon starts)', () => {
  it('asks again while the daemon is not there yet, then answers with the list', async () => {
    const proxy = vi.fn().mockResolvedValueOnce(notReady).mockResolvedValueOnce(notReady).mockResolvedValueOnce(listed)
    const waited: number[] = []
    const res = await getSkillList(proxy as never, async (ms) => { waited.push(ms) })
    expect(res).toEqual(listed)
    expect(proxy).toHaveBeenCalledTimes(3)
    expect(waited).toEqual([SKILL_LIST_RETRY_MS[0], SKILL_LIST_RETRY_MS[1]])
  })
  it('a daemon error that is no 「not there yet」 is final at once', async () => {
    const denied = { ok: false, status: 403, data: { error: { code: 'forbidden', message: 'x' } } }
    const proxy = vi.fn().mockResolvedValue(denied)
    const res = await getSkillList(proxy as never, async () => {})
    expect(res).toEqual(denied)
    expect(proxy).toHaveBeenCalledTimes(1)
  })
  it('gives up after the last wait, and says so', async () => {
    const proxy = vi.fn().mockResolvedValue(notReady)
    const res = await getSkillList(proxy as never, async () => {})
    expect(res.ok).toBe(false)
    expect(proxy).toHaveBeenCalledTimes(SKILL_LIST_RETRY_MS.length + 1)
  })
  it('a proxy that throws counts as not there yet', async () => {
    const proxy = vi.fn().mockRejectedValueOnce(new Error('ipc')).mockResolvedValueOnce(listed)
    expect(await getSkillList(proxy as never, async () => {})).toEqual(listed)
  })
})
