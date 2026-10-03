# 跨仓兼容矩阵

> 状态：P0 frozen

## 已冻结组合

| Agent24 | Open Design | Workspace | ACP profile | Desktop host | 状态 |
| --- | --- | --- | --- | --- | --- |
| `69baf50` baseline + A24-OD-00~09 | `open-design-v0.22.2@7395321` | `a24.workspace.v1` | `agent24.open-design.v1` | `agent24.creative-host.v1` | 目标组合；实现中 |

该行不表示 `69baf50` 已经实现相应能力；它定义 feature branch 的已审计主干基线和必须满足的目标组合。每项实现进入 `main` 后，用准确 merge commit 替换 baseline 表述。

## 协议兼容规则

- Agent24 自有 workspace/desktop-host contract 使用 major/minor；major 不同必须 fail closed，minor additive 字段必须可忽略。
- ACP 按官方整数 `protocolVersion` negotiation 与 capability advertisement，不发明 minor version。pin-specific interop 扩展由 `agent24.open-design.v1` profile 和真实 pinned-engine fixture 管理。
- 未知枚举值不得触发越权默认行为。
- `workspace_id` 是 opaque string。消费方只能比较和回传，不能解析 prefix 取得路径或权限。
- Open Design pin 必须同时匹配 tag 与 commit；tag 漂移视为供应链失败。
- packaged binary/resource 必须匹配 pin manifest 和 SHA-256；开发 override 必须显式开启并显示在状态中。

## CI 门禁

| 变化 | 必须运行 |
| --- | --- |
| Agent24 workspace/OpenAPI | Rust tests、OpenAPI validation、TS generation drift、contract fixtures、并发隔离与 restart tests |
| `agent24 acp` | ACP fixture、两轮 session、cancel、approval、WS reconnect、stdout purity |
| Open Design runtime | upstream baseline、runtime detection/model probe、ACP adapter tests、Creative smoke |
| Electron host | sidecar unit、navigation/CSP、view crash、sidecar crash、shutdown、dev/packaged smoke |
| upstream pin | 全部上述测试 + license/NOTICE/SBOM diff + 人工 release-note review |

## 上游升级流程

1. 创建独立 upstream-sync branch，不直接移动 integration branch pin。
2. 验证新 tag 指向的 commit，更新 source/build artifact checksum。
3. 运行无 Agent24 修改的上游基线测试，记录新增已知失败。
4. 运行 ACP profile、workspace E2E 和 Electron smoke。
5. 检查 license、bundled third-party inventory、runtime API 和 daemon 启动方式变化。
6. SOL review 通过后才能更新本矩阵。

回滚只需恢复旧 pin 和匹配的 bundled resources；不得让 packaged app 自动从网络抓取“最新版本”。
