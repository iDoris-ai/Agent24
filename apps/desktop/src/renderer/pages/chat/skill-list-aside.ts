// Test helper. The skill picker asks GET /api/v1/skills on mount; chat tests that count
// or order backendProxy calls keep the mock to themselves by putting this in front of it.
type Req = { method: string; path: string }
type Res = { ok: boolean; status: number; data: unknown }

export const skillListing: { respond: () => Res } = { respond: () => ({ ok: true, status: 200, data: { skills: [] } }) }

export function resetSkillListing(): void {
  skillListing.respond = () => ({ ok: true, status: 200, data: { skills: [] } })
}

export function aside<R>(proxy: (req: never) => R): (req: Req) => R | Promise<Res> {
  return (req) => (req.method === 'GET' && req.path === '/api/v1/skills' ? Promise.resolve(skillListing.respond()) : (proxy as (req: Req) => R)(req))
}
