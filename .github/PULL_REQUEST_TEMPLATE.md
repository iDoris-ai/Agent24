## Summary

<!-- 1-2 sentences -->

## Changes

<!-- bullet points -->

## 架构法律（[`docs/laws/`](../docs/laws/README.md)）

- [ ] 本 PR 改变可观察行为 → 已在下方点名受影响的法条（`L-XXX-n`），并说明凭什么符合（最好指向测试）
- [ ] 本 PR 与现有法条冲突 → 已改成符合，或另开一个只改法条的 PR（**不得**在同一个 PR 里既改法律又改行为）
- [ ] 本 PR 不改变可观察行为（纯重构 / 文档）→ 无需点名

受影响法条：

## Test plan

- [ ] `pnpm typecheck` passes
- [ ] `pnpm test` passes
- [ ] If touching main process: app launches without errors (`pnpm dev`)
- [ ] If touching renderer: UI renders + IPC ping works

## Related

<!-- ADR / related PRs -->
