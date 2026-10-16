# 独立只读复核：merge/kernel-to-main-20261016（K1 → main release 合并树）

- 复核对象：`origin/merge/kernel-to-main-20261016` @ `571190cb`（父提交 `9d1062cc` = ab/kernel 侧，`729a7159` = main 侧 #1015）
- 对照：`origin/main` @ `729a7159`，`origin/ab/kernel` @ `9d1062cc`
- 复核者：Grok Bot baton 车道（分支 `baton/grokbot-20261016-a24-k1review`）
- 方式：只读代码阅读 + git 对象比对 + 定向 `cargo test`；另在**未提交、已删除**的临时 worktree 里写了一个演示测试（代码附在附录 A，可原样复现）。没有修改任何生产代码。
- 下文 `文件:行号` 均指 `571190cb` 树内的行号。

> **声明：本报告不是审批，不替代外部 PR-Daemon 评审。** 它只回答 baton 任务提出的三问，结论供 PR-Daemon 与 jason 参考。

---

## 问 1：K1 consent/授权门在“停用/卸载撤销失败只记日志”下是 fail-open 还是 fail-closed？

### 1.1 代码事实

**撤销点（只记日志）**

| 位置 | 行为 |
|---|---|
| `rust/apps/agent24d/src/os_routes.rs:478-483` | `apply()` 在 `enabled=false` 时调用 `invalidate_module_consents`，失败只 `tracing::error!`，随后照常 `hand_off` 并返回成功。 |
| `rust/apps/agent24d/src/os_routes.rs:964-966` | `stop_now_os`（`POST /api/v1/os/{name}/stop`，CLI 卸载会调用）同样失败只记日志。 |
| `rust/apps/agent24-cli/src/main.rs:885` | `cmd_uninstall` 删除包后调用 `invalidate_module_consents_best_effort`。 |
| `rust/apps/agent24-cli/src/main.rs:835` | `os install` 成功后也做一次 best-effort 撤销（包替换场景）。 |
| `rust/apps/agent24-cli/src/main.rs:895-907` | best-effort 实现：`state_dir()` 为 `None` 时**静默 return，连提示都没有**（896-898）；打开库或写入失败只 `eprintln!`（902、905），卸载结果仍是 `Ok(())`（887）。 |
| `rust/crates/agent24-store/src/module_consents.rs:433-439` | `invalidate_module_consents` 本身注释写明“call site 上刻意 best-effort，失败返回给调用方记日志”。 |

**使用点（调用时的判定链）** —— `rust/crates/agent24-tools/src/lib.rs` `ModuleTool::call_with_invocation`：

1. `lib.rs:796-798` 先 `authorization.authorize(...)`，它走 `ModuleConsentAuthorization` → `StoreConsentGate::authorize`（`rust/crates/agent24-policy/src/consent_gate.rs:300-333`）→ `Store::lookup_module_consent`（`module_consents.rs:535` 起）。**这一层只看 consent 表与 revocation 表、版本、范围指纹、到期，完全不读 `os.json` 的 enabled，也不问模块是否在跑。**
2. `lib.rs:840-850` 再 `runtime.check_available(...)`：`AgentModuleToolRuntime::check_available`（`rust/apps/agent24d/src/module_tool_runtime.rs:65-78`）→ `generation()`（同文件 39-52），要求 supervisor 有该模块的 running slot 且 `DrainState::Running`，否则 `ModuleUnavailable`。

查询失败本身是 fail-closed 的：`consent_gate.rs:329-333` lookup 出错一律 `Deny(NotGranted)`；没有行也是 `NotGranted`（`module_consents.rs` lookup 末段）。

**所以对“consent 被使用时会不会再核模块是否启用”的回答是：consent 门本身不核；“是否启用”只由另一层（运行时 generation 是否 Running）间接把关。** 这两层在调用时取交集。

### 1.2 撤销失败的后果

- **同一 daemon 生命周期内、停止已落地的情况**：模块已不是 Running，第 2 步拒绝。即使 consent 还在，调用也过不去，这一段是 fail-closed。
- **停止没有落地时**：`apply()` 的 `hand_off` 结果可能是 `Pending`/`Failed`（`os_routes.rs:484-492`，以及 `settle`/`last_look` 路径）。如果这时撤销也失败了，模块仍是 Running，consent 仍是 Granted，两层都放行。也就是说 `os.json` 已写成 disabled，工具却还能被调用（双重失败窗口）。
- **重新启用后（主要问题）**：启用路径（`apply()` 中 `enabled=true`）不做任何 consent 处理。撤销失败留下的旧 consent 行，在 `os enable` 加 daemon 重启后会再次判为 `Granted`，用户不会被要求重新同意。卸载后用同版本重装也一样，前提是 CLI 卸载时的撤销和 `install` 时的撤销都失败（或 `state_dir()` 为 None 被静默跳过），且 daemon 不可达、`stop_now_os` 也没补上。
- **实证**：附录 A 的临时测试先 grant，再用 SQLite trigger 让 `module_consent_revocations` 插入失败，然后 `PATCH enabled=false`，最后恢复写入并 `PATCH enabled=true`。输出如下：
  ```
  K1R disable_status=200 OK reenable_status=200 OK lookup=Granted(ModuleConsentRecord { module: "consent-mod", op: "read_doc", ... decision: Granted, ... }) authorized=true
  test os_routes::tests::k1review_scratch_failed_invalidation_leaves_consent_authorized ... ok
  ```
  停用接口返回 200，os.json 已是 disabled，consent 却仍然 `is_authorized() == true`。
- **与已采纳设计的冲突**：
  - `docs/adr/ADR-K1-03-enable-authorization-trust-revocation.md:44`：“不得在未完成撤销的情况下报告成功”。
  - 同文件 `:46`：“撤销、禁用或过期时，应使关联能力/执行代次失效……若无法可靠取消或复验，执行必须 fail closed”。
  - 同文件 `:88`：“授权存储/撤销通知失败，统一否决为拒绝或不可用”。
  - 同文件 `:113`（6a.3）：“到期、禁用……触发失效/重同意”。

### 1.3 当前实际可利用性（降低严重度的事实）

在 `571190cb` 的生产代码里**没有任何写入 consent grant 的路径**：`grant_module_consent(` 只在测试里被调用（`module_tool_runtime.rs:414`、`server.rs:3550/3565`、`os_routes.rs:1993`、`module_consents.rs` 测试段）。路由层只有 list/export/revoke（`server.rs:1081-1092`）。因此今天任何生产库里都不会有 Granted 行，所有模块工具调用都是 `NotGranted` 拒绝。

### 1.4 判定

**生命周期撤销这一环是 fail-open（目前潜伏）。** 使用时对已停止的模块是 fail-closed，但那依赖“停止已落地”，并不依赖 consent 撤销。撤销失败只记日志，会让已停用模块的授权在重新启用（或同版本重装）后继续生效；停止未落地时，即刻就会继续生效。这违反 ADR-K1-03 §3 的 fail-closed 要求。目前因为没有生产 grant 路径，所以无法利用；一旦 grant UI 或路由合入，就会变成可利用的缺陷。PR-Daemon 在 #1016 记的 Medium 成立。

可选的修法（供实现方选择，本报告不改代码）：撤销失败时让 disable/stop 返回非 2xx，或把 `os.json` 回滚为 enabled；或者在启用路径或 daemon 启动时，对“os.json 曾为 disabled、但没有 revocation 行”的模块补撤销；或者在 `ModuleConsentAuthorization` 里同时校验 consent `decided_at` 晚于最近一次 enable。另外，CLI 在 `state_dir()==None` 时至少要报错，不能静默返回。

---

## 问 2：迁移单调性——合并后 main 侧 migrations 是否完整包含 ab/kernel 的 0015/0016/0017 且逐 blob 一致？

**结论：是。合并树的 migrations 目录与 ab/kernel 的 tree 对象完全相同；main 原有的 0001-0014 没有被修改，只新增了 0015/0016/0017。**

命令与结果（`M=rust/crates/agent24-store/migrations`）：

```
$ for f in 0015_module_consents.sql 0016_run_source_tags.sql 0017_module_consent_revocations.sql; do
    echo "$f kernel=$(git rev-parse origin/ab/kernel:$M/$f) release=$(git rev-parse 571190cb:$M/$f)"; done
0015_module_consents.sql kernel=34bb39b6e02648a077a60caede0d9eafd432af55 release=34bb39b6e02648a077a60caede0d9eafd432af55
0016_run_source_tags.sql kernel=617ce8cb90933d638ca856b2fbaba2e0d18691b6 release=617ce8cb90933d638ca856b2fbaba2e0d18691b6
0017_module_consent_revocations.sql kernel=d24f84027b74044af786f96b24a4b97dfe867d6c release=d24f84027b74044af786f96b24a4b97dfe867d6c

$ for r in origin/main origin/ab/kernel 571190cb; do echo "$r tree=$(git rev-parse $r:$M)"; done
origin/main tree=edfe37e4d81059fc7a5d49cadfe837ff6e424e75
origin/ab/kernel tree=0debd84168687845e6252e9a260ab3e8fd5d87f6
571190cb tree=0debd84168687845e6252e9a260ab3e8fd5d87f6        # 与 kernel 整个目录 tree 相同

$ git ls-tree origin/main $M/ | wc -l ; git ls-tree 571190cb $M/ | wc -l
14
17

$ git diff --name-status origin/main 571190cb -- $M
A	rust/crates/agent24-store/migrations/0015_module_consents.sql
A	rust/crates/agent24-store/migrations/0016_run_source_tags.sql
A	rust/crates/agent24-store/migrations/0017_module_consent_revocations.sql

$ git merge-base --is-ancestor origin/main 571190cb && echo ok      # ok
$ git merge-base --is-ancestor origin/ab/kernel 571190cb && echo ok # ok
```

main 上 0014 之后没有别的迁移，所以编号不冲突，单调递增成立。

---

## 问 3：回归面——release 相对 main 的 44 个文件是否改掉或删掉了 main 已有的行为？

总览：`git diff --stat origin/main 571190cb` 共 44 个文件，+10885/−161；按类型分 A=15、M=29、D=0（`--diff-filter`）。`git diff --stat origin/ab/kernel 571190cb` 只有 7 个文件，正好是 #1015 的 7 个文件。也就是说合并树相对 kernel 唯一的新增就是 main 侧的 #1015。

### 3.1 点名项

| 项 | 结论 | 证据 |
|---|---|---|
| `PASSTHROUGH_VARS` 18→19 | **保留 main 的 19 项**，没有被 kernel 的 18 项覆盖。`service.rs` 与 main 逐 blob 相同。 | main `[&str; 19]` blob `52a8e144`；kernel `[&str; 18]` blob `057fc5e3`；release `[&str; 19]` blob `52a8e144`。`rust/apps/agent24-cli/src/service.rs:120` 首项 `"AGENT24_DATA_DIR"`。`cargo test -p agent24-cli passthrough` 结果为 `passthrough_list_matches_what_the_daemon_actually_reads ... ok`，exit 0，说明 kernel 新代码没有引入未转发的环境变量。 |
| `state_dir()` | **保留 main（#1015）的实现**。`state_file.rs` 与 main 逐 blob 相同，K1 新增代码都通过它取目录。 | `state_file.rs` blob：main=`d195c100`，release=`d195c100`，kernel=`e7ba9b7a`。`rust/crates/agent24-protocol/src/state_file.rs:133-149`。K1 新调用点：`agent24-cli/src/main.rs:896`、`agent24d/src/server.rs:1534`（packages_root/catalogue 只是前移，没有删除，见 `server.rs:1534-1537`）。diff 里没有新增 `HOME`/`.agent24` 硬编码。 |
| #1015（MAINT-03，`729a7159`） | **完整保留**：它改的 7 个文件与 main 的 diff 为空。 | `git diff-tree --name-only -r 729a7159 \| xargs git diff --stat origin/main 571190cb --` 输出为空。 |
| #1014（desktop 错误码文案，`fdcf84d2`） | **完整保留**：它改的 3 个文件与 main 的 diff 为空。 | 同上命令换成 `fdcf84d2`，输出为空。 |

### 3.2 161 行删除逐文件结论

| 文件 | main 行为是否被改 | 说明 |
|---|---|---|
| `rust/apps/agent24d/src/routes.rs:559-571` | **改了（需决策）** | `/api/v1/chat` 原来用 `TaskProfile::default()`，注释写“local-first，只在需要时向外回退（D2）”。现在改为 `task_profile_for_run`，见下文 3.3。 |
| `rust/crates/agent24-agent/src/lib.rs:163-170, 238-241, 1720-1723` | **改了（需决策）** | agent loop 和会话压缩（summarizer）都改成按 run 来源政策选画像；会话压缩固定为 `LocalOnly`。main 原注释明确警告这样做“would silently stop compacting on remote-only setups”。 |
| `rust/crates/agent24-agent/src/subagent.rs` | **改了（需决策）** | explorer 子 agent 的模型调用同样收窄为 LocalOnly（没有 store 时也是 LocalOnly）。 |
| `rust/crates/agent24-policy/src/lib.rs` | 不影响 builtin/MCP | `decisions_for` 只对 `source=="module" && per_call_approval` 去掉常驻选项；builtin、MCP 的现有选项不变，有测试锁定（`module_per_call_approval_offers_no_standing_grants`）。 |
| `rust/crates/agent24-tools/src/lib.rs` | 小行为变化（低） | `with`/`with_unlisted` 改为拒绝重名注册，先注册的胜出（原来是后注册覆盖）；risk override 不得放宽的规则扩展到 `module`；Deny/Abort 时追加审计；超时和取消在 `isolates_cancellation()` 为 false 时行为与原来相同。 |
| `rust/crates/agent24-store/src/audit.rs` | 收紧（低） | `verify_audit_chain` 改为严格版，额外检测“被删掉的有效尾部”（与 AUTOINCREMENT 高水位比较）。生产代码不调用 `verify_audit_chain()`（只有测试调用）；严格版 `_tx` 只被新的 typed writer 使用。main 没有删除 audit_log 行的生产路径，存量库不受影响。 |
| `rust/crates/agent24-os-proto/src/kernel_call.rs` | 不变（非 strict 路径） | 新增 `strict_body` 与取消分支；`strict_body=false` 时保留“2xx 且 body 出错时忽略 body”的旧语义。 |
| `rust/apps/agent24d/src/server.rs` | 不变 | 删除行是 catalogue/packages_root 的前移和 explorer 构造方式（`new_with_store`）的变化。 |
| `rust/apps/agent24-cli/src/main.rs` | 不变（新增副作用） | `os_local` 改成 async；install/uninstall 额外做 consent 撤销（见问 1）。 |
| `rust/apps/agent24d/src/domain.rs` | 仅测试 | 限流测试改成有界循环，消除对挂钟的依赖。 |
| `rust/crates/agent24-store/src/{lib,legacy_recovery,allocation_reservation}.rs`、`Cargo.toml` | 不变 | `Store` 结构体新增字段，测试里的构造方式随之改动；依赖 feature 调整。 |
| `rust/crates/agent24-agent/src/session_memory_tests.rs` | 仅测试 | — |
| `docs/laws/APPROVAL.md`（L-APPR-5） | **法律文本被改** | 从“`write_local` MUST 永远逐次询问”改为允许第一方可逆草稿 MAY 免询。来源是 `62218c31`（K1-6a.5 实现提交，同一提交还改了 `consent_gate.rs` 等）。ADR-K1-03 第 120 行记载 jason 于 2026-10-08 授权了这一修订；但 `docs/agent/PLAN-KERNEL-K1.md:24,84` 要求“法律变更必须单独 PR，不得在实现 PR 中顺手改法律”。代码侧目前还没接入这个例外（法律文本自己写明“当前工具分发路径尚未接入该核验结构……缺失时按普通 `WriteLocal` 逐次审批”），所以运行行为不变，但这是流程问题。 |

### 3.3 LocalOnly 默认化对 main 的实际影响

- `SourceRef::user_input` 固定为 `LocalOnly`（`rust/crates/agent24-store/src/source_policy.rs:115-125`）。没有标签时也是 `LocalOnly`（同文件 `:199-206`）。查询失败同样回落到 `LocalOnly`（`agent24-agent/src/lib.rs:480-493`）。生产代码里**没有任何地方产生 `CloudAuthorized` 标签**（`rg SourceMode::CloudAuthorized` 只命中 from_tags 的分支和测试）。
- 因此合入后，main 上所有主对话、`/api/v1/chat`、explorer 和会话压缩都只能路由到 `Local/Lora`（`agent24-models/src/router.rs:86`）。而 `OMLX_URL`/`OLLAMA_URL` 一旦指向非 loopback 地址就会被标为 Remote（`router.rs:291-299`）。
- 后果：只有远端或局域网模型的部署（例如 oMLX 跑在另一台机器上），合入后对话会直接返回 unavailable，会话压缩也会停掉；而在 main 上这些部署可以向外回退（`router.rs:88`）。
- 这符合 ADR-K1-02 的设计（`docs/adr/ADR-K1-02-session-dataflow-security-modes.md:15,53`），**属于有意为之，不是实现缺陷**。但它确实删掉了 main 已有的行为，合入前需要明确接受并写进发布说明。

### 3.4 定向测试（如实记录）

| 命令（`rust/` 下，共享 target 目录） | exit | 结果 |
|---|---|---|
| `cargo test -p agent24-store module_consents` | 0 | lib 24 passed |
| `cargo test -p agent24-cli passthrough` | 0 | 1 passed |
| `cargo test -p agent24d --bin agent24d consent` | 0 | 6 passed（含 `disabling_a_module_persists_consent_invalidation`） |
| 附录 A 临时测试（未提交） | 0 | 1 passed，证明撤销失败后 consent 仍是 Granted |

没有跑全量测试，也没有跑 clippy 或 desktop 测试，这些不在本报告的结论范围内。

---

## 必须在合 main 前修掉 / 处理的清单

1. **[代码，Medium] 生命周期 consent 撤销失败时必须 fail closed**（问 1）。`os_routes.rs:478-483`、`os_routes.rs:964-966`、`agent24-cli/src/main.rs:895-907`（含 `state_dir()==None` 静默跳过）。依据是 ADR-K1-03 第 44、46、88、113 行。如果 jason 或 PR-Daemon 决定接受“生产 grant 路径合入前只是潜伏风险”，那至少要把它登记为 **grant 路径 PR 的硬阻塞项**，并在 tasks/followups 里点名。
2. **[决策，非代码] LocalOnly 默认化删除了 main 的远端回退**（问 3.3）。合入前需要 jason 明确接受“只有远端或局域网模型的部署对话不可用、会话压缩停止”，并写进发布说明；否则需要先提供 CloudAuthorized 的授权入口。
3. **[流程] L-APPR-5 法律文本修改混在实现提交 `62218c31` 中**（问 3.2）。需要确认 jason 2026-10-08 的授权是否等同于 `PLAN-KERNEL-K1.md:24,84` 要求的“法律单独 PR”，或者补一个单独的法律 PR 或记录。

除以上三项外，问 2（迁移）与问 3 的点名项（PASSTHROUGH_VARS、`state_dir()`、#1014、#1015）**没有发现需要修的问题**。

> **再次声明：本报告不是审批，不替代外部 PR-Daemon 评审。**

---

## 附录 A：演示测试（未提交，仅供复现）

放进 `rust/apps/agent24d/src/os_routes.rs` 的 `mod tests` 中，然后运行 `cargo test -p agent24d --bin agent24d k1review_scratch -- --nocapture`：

```rust
#[tokio::test]
async fn k1review_scratch_failed_invalidation_leaves_consent_authorized() {
    let (mut st, _dir, path) = t8_state().await;
    st.os_reports = std::sync::Arc::new(vec![report("consent-mod", MountOutcome::Mounted)]);
    crate::os_config::OsConfig::set_enabled(&path, "consent-mod", true).unwrap();
    let summary = agent24_store::ToolPermissionSummary::new(
        "consent-mod", "read_doc", "1.0.0",
        agent24_store::ConsentSource::FirstParty,
        Some("docs".into()), None, None,
        agent24_store::HostRiskLevel::Low,
    );
    st.store.grant_module_consent(&summary, "2026-10-08T00:00:00Z", "2099-11-08T00:00:00Z").await.unwrap();
    sqlx::query("CREATE TRIGGER k1r_fail BEFORE INSERT ON module_consent_revocations BEGIN SELECT RAISE(ABORT, 'simulated write failure'); END")
        .execute(agent24_store::test_hooks::pool(&st.store)).await.unwrap();
    let response = t8_patch(&st, "consent-mod", false, Some(path.clone())).await;
    let status = response.status();
    assert!(!crate::os_config::OsConfig::load(&path).unwrap().is_enabled("consent-mod"));
    sqlx::query("DROP TRIGGER k1r_fail").execute(agent24_store::test_hooks::pool(&st.store)).await.unwrap();
    let re = t8_patch(&st, "consent-mod", true, Some(path)).await;
    let lookup = st.store.lookup_module_consent(&summary.module, &summary.op, &summary.module_version,
        &summary.scope_fingerprint(), "2026-10-16T00:00:00Z").await.unwrap();
    println!("K1R disable_status={status} reenable_status={} lookup={lookup:?} authorized={}", re.status(), lookup.is_authorized());
    assert_eq!(status, StatusCode::OK);
    assert!(lookup.is_authorized());
}
```
