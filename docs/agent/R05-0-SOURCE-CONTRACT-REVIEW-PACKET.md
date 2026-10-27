# R05-0：来源契约外审包

> **只列待决，不批准实现。** 2026-10-24。基线 `ab/memory-p1@a09561c2`（含 #1154）。对照 `docs/adr/ADR-MEMORY-RECALL-MESSAGE-ORIGINS.md`（`e196be4b`，仍为 DRAFT）§4/§5 与 `docs/agent/PLAN-MEMORY-P1.md`。本文件不改 migration、不指定下一迁移号、不派 H03、不启动 purge 或 #794。

## 1. 已核实，不再当开放问题

- H02 最小修复已在集成分支：#1152 同秒审计 ID、#1153 H02-a1、#1154 H02-a2（head `9ad18683`，clestons APPROVE，required CI 全绿）。H02 不依赖本契约。
- 现行恢复承诺仍是：审批恢复重放已存 snapshot，不按新记忆开关重决定；撤回只裁请求副本，不回写 snapshot。依据 `docs/agent/PLAN-M1-MEMORY-HARDENING.md` 与 OpenAPI `putMemorySettings` 豁免。P01 已列冷恢复场景按此核销，不要求改暂停契约。
- 归档 #1094 / `PLAN-M1-SNAPSHOT-PROVENANCE.md` 的合并不等于契约批准，不继承其 `0015` 迁移号。store 现到 `0018`；`0016_run_source_tags` 只做 K1 隐私路由，不证明 host recall。禁止把缺映射或旧 snapshot 回填为 NoRecall。
- 分支手记已记录用户对无可信来源旧 run 的行为决定：暂停 provider/审批恢复，提示显式新开 run；原文、history、snapshot 保留；不自动复制不明记忆，不重放旧工具。该行为门不再向用户重问。private schema 与公开兼容仍要独立外审。见 `docs/agent/HANDOFF-MEMORY-HARDENING.md`。

## 2. 本包要外审回答的唯一产品问题

**R05 §4 的建议是否收紧了现行 snapshot 恢复承诺？**

相对「审批恢复按已存 snapshot 继续发送」，是的。§4 建议：旧/坏来源且无法证明来源闭包的 run 不自动恢复、不自动发送；历史仍可见，并返回需新建会话。用户可在新会话重输自己的正文。不能自动复制旧 recall 或待执行工具，旧审批 grant 不转移到新 run。

这与手记里已解除的 legacy 行为门同向，但 R05 正文仍写明：不能用安全措辞绕过现行 snapshot 恢复承诺，也不能把「所有旧 run 立即失效」当成已选上线开关。因此外审要确认的是精确文本，不是再问用户要不要停旧 run。

## 3. 请外审按条接受、收窄或拒绝

| ID | 待决 | 不接受则不能做的事 |
|---|---|---|
| D1 | 收紧范围只限「缺映射 / 坏来源 / 无法证明来源闭包」的 run，不是所有旧 run | 不得把已有可信来源的审批恢复改成一律停发 |
| D2 | 可见历史、原文、snapshot 保留；只停自动恢复/发送，并给出新建会话回执 | 不得删历史来换安全 |
| D3 | 不自动复制旧 recall、待执行工具；旧审批 grant 不进新 run | 不得把隔离实现成静默重放 |
| D4 | 无损升级旧 run 必须另有可信写点凭证/迁移证明；禁止 marker、位置 0、role 或同文回填 | 不得因迁移成本退回文本推断 |
| D5 | 新来源映射与 K1 `0016` 正交；实施前重核最大迁移号；本包不选定表名以外的 SQL | 未获本包精确 head APPROVE 前不得改 `rust/crates/agent24-store/migrations/` |
| D6 | §5 矩阵是批准后的验收，不是本片测试结果。同文三来源、冷恢复不靠内存缓存、缺失/坏 digest/读库失败必须拒发、normalize 前校验、K1 LocalOnly/Unknown 不因 UserPrompt 变 CloudAuthorized | 不得用本文宣称 P02 红证已绿 |

建议决定与 §4 草稿一致：接受 D1–D4 作为兼容策略，D5/D6 作为实现门。外审可以收窄 D1。本稿不代外审打勾。

## 4. 批准后的下一独立切片（本片不实现）

精确 head APPROVE 且 D1–D4 未被拒绝之后，下一片才是存储原子接口与阴性夹具：新 run 消息与来源同一事务，失败整批 rollback；缺行不是 NoRecall。仍不派 H03 消费者，不改 purge，不碰 #794。该片默认总变更不超过 300 行，过渡态受控不启用。

## 5. 本片验收

- 只新增本文件。
- `git diff` 不含 `migrations/`、Rust、协议或 OpenAPI。
- 未把 R05 从 DRAFT 改成已批准，未勾 PLAN 的 R02/R05 或 H03。
