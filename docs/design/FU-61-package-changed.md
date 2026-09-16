# FU-61 —— 包在运行中被卸载或原地替换

> 状态：设计稿 v5（v1 → v2：Codex 第 1 轮 6 条 High、5 条 Medium、1 条 Low 全部采纳；v2 → v3：第 2 轮 1 条 High（部分）、3 条 Medium、2 条 Low 全部采纳；v3 → v4：第 3 轮 2 条 High（1 条全新的 tombstone、1 条 stop oracle 仍破）、1 条 Medium（launchd 脱管，记为 FU-68）、1 条 Low 全部采纳；v4 → v5：第 4 轮 1 条 Medium（`stop_now_os` 漏了 `last_look` 复核）、4 条 Low 全部采纳，见文末各段改动记录）。来源：`followups.md` FU-61；ME3-NEXT 执行队列第三段（2026-09-13 用户裁决：按建议「检测变更、报需要重启」+ `uninstall` 先热 disable，不做快照）。
> **带状态机：先写语义说明，再写代码**（`docs/agent/tasks.md` 的硬规则，SUP-5 的教训）。

## 问题

Supervisor 在 `mount()` 时读一次包（`discovery::scan()`），把 `package_dir` 和 `manifest_digest` 冻结进 `ModuleSpec`，此后整个生命周期——包括每一次崩溃重启——都拿着这份冻结的值，从不重新核对磁盘。两个操作能让磁盘状态和这份冻结值分道扬镳：

1. **`agent24 os uninstall`**：文件被移到 staging 再删除（`install.rs::uninstall`）。今天 `uninstall` 只操作文件、完全不碰正在跑的 daemon（这是刻意的——见下文「不改的东西」）。CLI 自己的提示已经如实写了后果：`(a module of it running now keeps running, but cannot be restarted if it exits)`。
2. **原地替换**：同一个 `package_dir` 换了内容（常见于「装个新版本到同一个目录」）。**本设计只能检测清单文件本身的变化**（见下文「范围收窄」）。

这两种情况今天的后果分两段：

- **模块健康、从不重启**：uninstall 或替换发生后，模块**继续服务**——对一个长期健康运行的模块，这个落差可能永远不会自己浮现。
- **模块崩溃、触发重启**：`run_once` 照旧用冻结的 `spec` 重新 spawn，包被卸载时 spawn 在 `resolve()` 失败（`Setup`），包被替换时握手报 `manifest_mismatch`（`Refused`）——两者都按 backoff 重试 5 次后触发熔断（`GaveUp`），诊断质量差、且替换的情况每次退避到期都要白白重新 spawn+握手一次，这个循环不可能自愈。

## 根治二选一（用户已裁决，选 2）

1. 每个 daemon 把包树快照成不可变副本——**不做**（记为后续可选项）。
2. 显式检测变更、报「需要重启」，不做快照——**本设计走这条**。

## 范围收窄（v2 新增，回应 Codex Medium 2）

`Discovered::digest`只是 `domain-os.yml` 这一个文件的精确字节摘要（`discovery.rs`）。**本设计能检测到的「包变了」严格等于「清单文件的字节变了，或包目录本身不再可用」**——如果操作者只换了模块的可执行文件/脚本/资源、清单一字未改，`recheck` 会判定「未变」，下次重启会照常用新的可执行文件跑起来（这不是新引入的漏洞：今天不检测任何东西，这个残余只是「检测范围有多宽」的问题,不是「有没有检测」的问题）。全文档避免使用「包变了」这种更宽的措辞，统一说「清单变了或包目录不再可用」。给包整棵目录树建身份（例如整树摘要）是一个更大的改动,不在这次范围内,不做。

## 改成什么

### A. 重启前核对清单（回应「模块崩溃后重启」这一段）

**新增** `agent24-os-packages::discovery::recheck`（和 `read_package` 同文件，直接调用私有函数，不需要放宽任何可见性）：

```rust
/// Re-checks the package at `dir` against `expected` (a `ModuleSpec`'s
/// manifest digest, frozen at mount time). `Ok(())` means manifest-
/// consistent under THIS check's scope — not a guarantee that nothing
/// about the package changed (v3, Codex round 2 Low: a changed
/// executable/script/asset behind an unchanged manifest passes this and
/// is NOT "safe to restart against" in the fuller sense that phrase
/// implies). `Err(reason)` covers: the directory no longer exists; it (or
/// its manifest) is no longer a safe entry to read (mirrors `scan`'s own
/// directory-level symlink refusal — v2, Codex High 4: `read_package`
/// alone only re-validates the MANIFEST file, not the directory it lives
/// in, and `scan()` refuses a symlinked package directory one level up
/// from where `read_package` starts reading); or the manifest's digest no
/// longer matches. Does not detect a changed command/script/asset whose
/// manifest bytes are unchanged — see the design doc's "范围收窄" section.
pub fn recheck(dir: &Path, expected: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.is_symlink() => {
            return Err(
                "the package directory is now a symlink; refusing to follow it".to_owned(),
            );
        }
        Ok(m) if !m.is_dir() => {
            return Err("the package directory is no longer a directory".to_owned());
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("the package directory no longer exists".to_owned());
        }
        Err(e) => return Err(format!("could not check the package directory: {e}")),
    }
    match read_package(dir) {
        Ok(found) if found.digest == expected => Ok(()),
        Ok(found) => Err(format!(
            "the manifest changed ({expected} \u{2192} {})",
            found.digest
        )),
        Err(why) => Err(format!("the package is no longer usable: {why}")),
    }
}
```

`agent24-os-proto` 不获得对 `agent24-os-packages` 的新依赖边——`ModuleSpec`（`supervisor.rs`）今天已经把 `package_dir: PathBuf` / `manifest_digest: String` 当**不透明值**持有。核对逻辑以一个不透明闭包的形态注入：

```rust
/// Re-checks a module's installed package right before restarting it
/// (FU-61): does `spec.package_dir` still hold a package whose manifest
/// digests to `spec.manifest_digest`? `None` if yes; `Some(reason)`
/// (human-readable, goes straight into `Status::PackageChanged`) if not.
/// Synchronous and does blocking filesystem I/O (v2, Codex Medium 1) — the
/// caller in `run_loop` runs it through `spawn_blocking`, not directly on
/// the async task. Built in `agent24d`, handed in as an opaque `Fn` so
/// this crate keeps not knowing what a "package" is.
pub type PackageCheck = Arc<dyn Fn(&ModuleSpec) -> Option<String> + Send + Sync>;
```

`domain.rs::mount()` 在已有的 `supervise(spec, dir, current, methods, timings)` 调用上多传一个参数：

```rust
let package_check: agent24_os_proto::supervisor::PackageCheck = Arc::new(|spec| {
    agent24_os_packages::discovery::recheck(&spec.package_dir, &spec.manifest_digest).err()
});
```

**注入点（v2 改写，回应 Codex High 1）**：v1 把检查放在退避睡眠**之前**，Codex 指出这样一个在退避期间才发生的删除/替换，要等到**下一次**退避到期才会被抓到（睡眠前检查完时改动还没发生，睡眠结束直接进入下一次 `run_once`，检查代码根本没有再跑一遍的机会）。**改成放在退避睡眠**之后**、下一次 `run_once` 之前**——`Decision::RestartAfter` 分支里 `tokio::select!`（等 stop 或睡完 `delay`）跑完、还没进入下一圈循环的那一刻：

```rust
Decision::RestartAfter(delay) => {
    slot.install(Generation::starting());
    // …existing status.send_replace(Status::Backoff { … })…
    after = Some(failure);
    tokio::select! {
        biased;
        () = stop_requested(&mut stop) => { /* unchanged */ }
        () = tokio::time::sleep(delay) => {}
    }
    if let Some(reason) = check_package(&package_check, &spec).await {
        tracing::error!(module = %spec.name, %reason, "not restarting: package changed");
        slot.retire();
        status.send_replace(Status::PackageChanged { reason });
        stop_requested(&mut stop).await;
        saw_stop(record, &stop, ProcessAtStop::None);
        slot.stopped(status);
        return;
    }
    // falls through to the loop top, attempt += 1, run_once(...) again
}
```

```rust
/// Runs `check` off the async task (Medium 1: `recheck` does blocking fs
/// I/O — `symlink_metadata`, a bounded read, YAML parse — bounded but not
/// appropriate on a Tokio worker). `ModuleSpec` is `Clone`; cloning it and
/// the `Arc<dyn Fn>` to move into `spawn_blocking` is cheap next to the
/// I/O itself. A panicked closure is reported as a package-changed reason
/// rather than propagated — this check must never bring the supervisor
/// loop down.
async fn check_package(check: &PackageCheck, spec: &ModuleSpec) -> Option<String> {
    let check = check.clone();
    let spec = spec.clone();
    tokio::task::spawn_blocking(move || check(&spec))
        .await
        .unwrap_or_else(|e| Some(format!("the package check panicked: {e}")))
}
```

**残余的 TOCTOU 窗口（v2 新增，Codex High 1 的后半句）**：这次检查和下一次 `run_once` 真正 `spawn` 之间仍有一个（很小的）窗口——检查通过之后、spawn 之前的一瞬间包被替换，这次重启仍然可能对着新文件跑。**这个设计不承诺消灭这个窗口**（要做到那一步需要在检查和 spawn 之间持有一个打开的目录 fd 或做快照，属于用户已明确裁决不做的「快照方案」的范畴）——它承诺的是「不会在一个已经确认变了的包上死循环 backoff/熔断」，不是「杜绝任何时序意义上的竞态」。这条边界写在这里，供 review 和以后的读者对照，不是遗漏。

**和 `stop_now_os` 之间的另一条窄竞态（v5 新增，回应第 4 轮 Low）**：如果 `check_package` 正好在退避睡眠之后跑（模块自己崩溃、进入了下一次重启前的核对），而这时 Part B 的 `stop_now_os` 刚好也对同一个模块发来一次热停请求——`check_package` 有可能先发布一次 `PackageChanged`，随后才被热停的 `slot.retire()`/`Stopped` 覆盖掉。这不是安全问题（不会有额外的 spawn 发生，最终状态仍然正确收敛到 `Stopped`），只是操作者可能在 `agent24 os list` 上瞬间看到一次本可以不出现的 `PackageChanged`——「一个已经被热停的模块不会再触发 `PackageChanged`」这句话因此要加一个「稳定态下」的限定，不是任何时刻都成立的绝对保证。这条窄窗口不专门修（修法是让 `check_package` 在发布状态前再看一眼 `stop` 通道，为一个纯展示层面的瞬时噪音换取额外的复杂度不值得），只在这里如实记录。

**为什么核对不算作 `RestartPolicy` 的一次失败**：核对跳过的是「要不要重新 spawn」这个决定本身，不产生新的运行、不产生新的 `RunFailure`,`policy` 的计数器保持不变。

**新状态**（`supervisor.rs::Status`，和 `GaveUp`/`Stopped` 并列）：

```rust
/// FU-61: the installed package's manifest changed or its directory
/// became unusable since this module was mounted, caught right before
/// what would have been the next restart. `reason`: what `recheck` said.
PackageChanged { reason: String },
```

它复用 `GaveUp` 一模一样的收尾时序（`slot.retire()` → 发布状态 → 等外部 `stop` → `saw_stop` → `slot.stopped(status)` 把状态最终收成 `Stopped`）。

**代理侧不需要新代码**：`slot.retire()` 撤销 `Generation`，代理按 `Revoked` 状态出既有的 `503 module_stopping`——`GaveUp` 今天已经是这条路径。**这条设计只改变操作者可见的诊断文本**，不新增 HTTP 错误码。

**操作者可见文本（v2 改写，回应 Codex High 5 与 Low 1）**：v1 建议的恢复办法「重启 daemon，或 `agent24 os disable` / `enable`」是错的——`os disable` 把这个模块的 `SupervisorHandle` 从运行中的注册表里整个撤走（`domain.rs`），`os enable` 只改 `os.json` 里的配置，**不会**在运行中的 daemon 里重新造一个 supervisor（这需要一次真正的「热重挂载」，是一个更大的、这次没有裁决要做的状态机改动，见「不改的东西」）——按 v1 的说法操作，模块会一直停到 daemon 下次启动,和「已经解决」的假象正相反。**唯一真实有效的恢复路径是重启 daemon**。同理,可见位置的说法收窄为 `agent24 os list` / `GET /api/v1/os`（v1 提了「`daemon status`」，但 `agent24 daemon status` today 只读健康和停机报告，不渲染 `DomainOsList`）。

**恢复命令本身要先能兑现（v3 新增，回应 Codex 第 2 轮 High 5 的后半句）**：`agent24 daemon stop && agent24 daemon start` 这句话本身**不是**这次新编的——`os_routes.rs`/CLI 里已有的 `restart_required` 提示（`print_os`）今天就是这么写的。但 Codex 指出它其实不可靠：`daemon stop` 只是 `POST /shutdown` 拿到 `202` 就打印「shutdown requested」并返回成功——**不等待进程真的退出**；紧接着的 `daemon start` 如果撞上一个还没退出的旧进程持有的单例锁,只会去轮询「是不是有别的 daemon 已经起来了」（`main.rs` 现有的并发抢跑兜底逻辑），**不会**在锁被释放之后重新尝试 spawn——两条命令中间没有任何东西保证旧进程已经完全退出,复合命令可能以「旧的没停干净、新的没起来」收场。这不是 FU-61 独有的问题（`restart_required` 提示今天就已经在建议一句不可靠的话），但 FU-61 把这句话当作**唯一**的恢复路径,不能继续放着它不可靠。**第 C 节**把这个修好——修好之后,这句话对 `restart_required` 和 `PackageChanged` 两处都变得可靠,不是只为 FU-61 单独打的补丁。

### C. `agent24 daemon stop` 要等到「`start` 真正在意的那个条件」满足再回成功（v4 改写）

**v3 的问题（Codex 第 3 轮 High，「STILL BROKEN」）**：v3 拿 `health_ok()` 变假作为「已经停了」的证据。但 `POST /shutdown` 一收到就触发 Axum 的 graceful shutdown——**监听器几乎立刻停止 accept**，`health_ok()` 因此很快就会失败,而这时旧进程往往还没退出：daemon 还在排空/停止模块、把停机汇总落盘、持有单例锁,直到 `serve()` 真正返回、进程退出为止。`health_ok` 变假只证明「新连接进不去了」，不证明「单例锁已经释放」——而后者才是 `daemon start` 真正需要的条件。v3 因此可能打印 `stopped` 返回成功，而紧跟着的 `start` 还是撞上同一把没释放的锁。

**v4 改法：直接探测 `start` 会用的那把锁,而不是拿一个间接信号猜**。`agent24-protocol::state_file::try_acquire_singleton()` 已经是公开函数（`agent24d` 自己在启动时用它防止双实例），`agent24-cli` 已经依赖 `agent24_protocol`——直接复用同一把锁做探测,拿到就立刻释放（不持有,只是为了证明「拿得到」），这正是 `daemon start` 接下来会做的同一件事：

```rust
Ok(r) if r.status().is_success() => {
    // `POST /shutdown` returns 202 the instant shutdown is REQUESTED；health
    // going false only means the accept loop closed, not that the process
    // has exited or released its singleton lock (Codex round 3: v3's
    // `health_ok`-based oracle could report `stopped` while the old
    // process still held the lock `start` needs). Poll the SAME lock
    // `start` will contend for, not a proxy for it.
    let deadline = tokio::time::Instant::now() + STOP_CONFIRM_BUDGET;
    loop {
        match agent24_protocol::state_file::try_acquire_singleton() {
            Ok(Some(lock)) => {
                drop(lock); // release immediately — this call only probes
                println!("stopped (pid {}, port {})", state.pid, state.port);
                return Ok(());
            }
            Ok(None) => {}
            Err(e) => return Err(format!("could not check whether it stopped: {e}")),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "shutdown requested (pid {}, port {}) but it did not release its lock within \
                 {}s — check its logs before starting a new one",
                state.pid, state.port, STOP_CONFIRM_BUDGET.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
```

`try_acquire_singleton()` 是同步的、非阻塞的文件锁探测（`try_lock_exclusive`，不是等锁——拿不到立刻返回 `Ok(None)`，不会让这次探测本身卡住），直接跑在 async 任务上和 `agent24d` 自己在启动时的用法一致,不需要 `spawn_blocking`。

**`STOP_CONFIRM_BUDGET`（v4 修正取值理由，回应 Codex 第 3 轮 Low）**：`lifecycle.rs` 里真实的停机上限是排空 ≤10s + 停止宽限 ≤5s + 汇总落盘 ~200ms + 运行时收尾余量 ~300ms ≈ 15.7s——v3 说「多个模块可能顺序停止」是错的（`server.rs` 用 `JoinSet` **并发**停止所有模块，不是排队),不需要按模块数放大;`try_acquire_singleton` 探测本身是本地文件锁,没有 v3 那种「`health_ok` 自带 3 秒超时,叠加到总预算上」的问题。30s 相对 15.7s 的真实上限依然留了近一倍余量，取值不变，理由改对。

**对 launchd 托管的 daemon 不适用（v4 新增,回应 Codex 第 3 轮 Medium，记为新跟进项）**：`agent24 service install` 把 daemon 交给 launchd 管（`KeepAlive`，异常退出自动拉起）；`agent24 daemon stop` 走的是 `POST /shutdown`，进程是**正常退出**（`SuccessfulExit:false` 语义下 launchd 认为「这次退出不需要拉起」)，随后 `agent24 daemon start` 用的是手动 spawn 路径,不经过 `launchctl`——新起的进程脱离了 launchd 的监管，之后崩溃不会再被自动拉起。**这不是 FU-61 引入的新问题**——`os_routes.rs` 现有的 `restart_required` 提示今天就在建议同一句 `agent24 daemon stop && agent24 daemon start`，同样受这个限制；FU-61 只是让这句命令本身变得可靠（旧进程真退出、新进程真起来），不改变它「会让托管中的 daemon 脱离 launchd」这个既有问题。记为新的跟进项 **FU-68**（见 `followups.md`），本设计不解决它。

这一条**只改变 `daemon stop` 的返回时机和成功文案**（`shutdown requested` → `stopped`，且真的等到确认），`daemon start` 的重试逻辑不用动：`stop` 可靠地等到旧进程释放单例锁之后，`start` 面对的就是一个空锁,今天已有的正常 spawn 路径直接成功,不会走到「撞锁轮询」那个分支。

`apps/agent24d/src/os_routes.rs` 的 `match status { ... }` 新增一支：

```rust
Status::PackageChanged { reason } => (
    "degraded",
    Some(format!(
        "package changed: {reason} — restart the daemon to pick up the current package \
         (`agent24 daemon stop && agent24 daemon start`)"
    )),
),
```

### B. `uninstall` 主动热停（回应「模块健康、从不重启」这一段，v2 大改，v4 再改热停机制本身）

**v1 的问题（Codex High 2/3/6）**：v1 提议在文件删除**之前**先 `connect()` 尝试热停。三个问题：（1）`connect()` 不是「尽力而为的探测」——它在没有健康 daemon 时会真的**启动一个新的临时 daemon**（`spawn_daemon(true)`），意味着 `os uninstall` 离线执行时可能把正打算删除的那个坏包**跑起来**；（2）热停的 `PATCH` 先把 `os.json` 写成 `enabled:false`、再交出停止，如果**之后**文件删除失败,会留下「daemon 里已停、`os.json` 已禁用、文件却还在」这种没有热 enable 能收拾的半途状态；（3）v1 的伪代码完全没看 `PATCH` 的响应状态,把「TCP 连上了」误当成「已经停掉了」。

**v3 复用现有 `PATCH /api/v1/os/{name}` 的问题,Codex 第 3 轮抓到的新 High（严重，回应见下）**：v3 把热停实现成对现有的 `PATCH /api/v1/os/{name}` 发一次 `enabled:false`——这条 RPC 除了停止正在跑的 supervisor,还会把 `enabled:false` **持久写进 `os.json`**（`os_routes.rs::apply`）。对一个即将从磁盘上彻底消失的包名来说,这个持久化是**有害**的：daemon 下次启动时,`discovery::scan` 再也发现不了这个包,`os_config.rs::unknown_disabled()` 会把「`os.json` 里写着 disabled、但这次构建根本不提供」的条目视为危险的拼写错误——这条判据是刻意设计的 fail-closed 保护（防止「以为关掉了、其实一直开着」这种更危险的沉默失败），**不能为了这次的场景削弱它**。后果是：`uninstall` → 热停 → 重启 daemon 这条最正常不过的路径,会让 `mount_all()` 判定整个注册表非法,**把所有其它本该正常挂载的模块一起降级成 503**——这不是这一个包的问题,是全局性的。

**v4 改法：热停不能复用会持久化的 `PATCH`,需要一个真正「只影响这一刻,不影响下次启动」的操作**。新增服务端路由 `POST /api/v1/os/{name}/stop`（`os_routes.rs`），直接复用已有的、和「要不要写 `os.json`」正交的两个内部函数 `hand_off`/`settle`（`patch_os` 内部已经是这么分层的——`apply()` 先写配置、再调 `hand_off`；新端点只调 `hand_off`，跳过配置写入这一步）：

```rust
/// Hands a running module's stop off RIGHT NOW, without writing `os.json`
/// (FU-61 — reusing `patch_os` here was the round-3 bug: persisting
/// `enabled:false` for a name about to vanish from discovery entirely trips
/// `unknown_disabled`'s fail-closed check on the daemon's next start and
/// takes the WHOLE registry down, not just this package). Same "refuse an
/// unknown name" guard `patch_os` has (this daemon's own mount report, a
/// snapshot from startup, still lists a package whose files were just
/// deleted — that is expected and fine, uninstall is exactly the moment
/// this route exists for), same `HotStop`/`settle`/`last_look` machinery
/// (v5: `last_look` is not optional here — a one-shot `settle()` alone
/// would miss a `Stopping` that becomes `StopFailed` moments later, or a
/// `Pending` that closes admission just after its own timeout, and answer
/// 2xx for a stop that did not actually land), same error codes for
/// `Failed`/`Pending` — the only thing genuinely new here is "skip the
/// config write."
pub async fn stop_now_os(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    if !state.os_reports.iter().any(|r| r.name == name) {
        let known: Vec<&str> = state.os_reports.iter().map(|r| r.name.as_str()).collect();
        return error_response(
            StatusCode::NOT_FOUND,
            "not_found",  // v5: matches `patch_os`'s existing code exactly
            &format!("no domain OS named {name:?}; known: {known:?}"),
        );
    }
    // v5 (Codex round 4 Medium): `settle()` alone samples supervisor status
    // ONCE. `patch_os` deliberately re-checks with `last_look()` afterward —
    // a `Stopping` that becomes `StopFailed`/`Panicked`/`Killed`, or a
    // `Pending` that closes admission just after its own timeout, would
    // otherwise be missed and this route would answer 2xx for a stop that
    // did not actually succeed. Reuse the identical shape, not a
    // hand-rolled one-shot read.
    let handed = hand_off(state.supervisors.as_deref(), state.shutdown.modules_cut_off(), &name);
    let slot = handed.as_ref().map(|(_, d)| d.clone());
    let hot = last_look(settle(handed, ADMISSION_CLOSED_WITHIN).await, slot.as_ref());
    match hot {
        HotStop::Failed => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "stop_failed",
            &format!("could not stop {name:?} cleanly; `agent24 os list` shows why"),
        ),
        HotStop::Pending => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "disable_pending",
            &format!(
                "asked {name:?}'s supervisor to stop it, but it still admitted requests after \
                 {ADMISSION_CLOSED_WITHIN:?}; `agent24 os list` shows when it has stopped"
            ),
        ),
        HotStop::Stopping | HotStop::Already | HotStop::NotRunning => render(&state),
    }
}
```

路由（`server.rs`，紧挨着既有的 `PATCH /api/v1/os/{name}`）：

```rust
.route(
    "/api/v1/os/{name}/stop",
    axum::routing::post(crate::os_routes::stop_now_os),
)
```

`os.json` 里这个名字原本是否已经有一条配置（比如操作者以前手动 `disable` 过它）——这条本身是既有的、和 FU-61 无关的残余（今天手动 disable 一个包再 uninstall 再重启,一样会撞上 `unknown_disabled`）；v4 只保证「FU-61 新加的这一步不会新制造一个这样的条目」，不负责清掉可能早就存在的旧条目，这条写进「不改的东西」。

**v2 → v3：pseudocode 换成真的能编译的形状（回应 Codex 第 2 轮 Medium「返回类型对不上」）**。`os_local()` 今天的签名是 `Option<Result<(), String>>`（`Install`/`Uninstall`/其它统一走这个洞），`Uninstall` 分支内部已经把 `install::uninstall` 返回的 `Result<bool, InstallError>` 里的 `bool`（真的删了 vs 本来就没装）**丢掉**、折叠成 `()`——这恰恰是热停判断「这一次是不是真的删除了什么」所需要的信息。硬把热停塞回 `os_local()` 内部会丢这个信息（可能对着「本来就没装」发一次没意义的 PATCH），塞回通用的 `Option<Result<(), String>>` 洞里也没法多带一个 `bool` 出去。**改法：`Uninstall` 不再走 `os_local()` 这条通用路径**，`cmd_os()` 对它单独分派到一个专门的 async 函数，`os_local()` 本身收窄成只处理 `Install`（同时保留一个小的、两处共用的 `packages_root()` 辅助函数，避免解析包根的逻辑被复制一份）：

```rust
async fn cmd_os(action: OsAction) -> Result<(), String> {
    if let OsAction::Uninstall { name } = &action {
        return cmd_uninstall(name).await;
    }
    if let Some(done) = os_local(&action) {
        return done;  // still handles Install
    }
    // …existing daemon-backed List/Enable/Disable path, unchanged…
}

/// `agent24 os uninstall` — file removal decides success or failure by
/// itself (unchanged contract: works with the daemon down); hot-disable is
/// a best-effort step tried ONLY after a real removal, whose outcome can
/// only add an informational line, never flip the `Result` this function
/// already decided.
async fn cmd_uninstall(name: &str) -> Result<(), String> {
    let root = packages_root()?;  // the `resolve_packages_root(...)` call
                                    // `os_local` already makes today, pulled
                                    // out so both paths share it
    match agent24_os_packages::install::uninstall(name, &root) {
        Err(e) => Err(e.to_string()),  // nothing removed: no PATCH, ever
        Ok(false) => {
            println!("{name} was not installed; nothing to remove");
            Ok(())  // idempotent: also no PATCH — there is nothing new to disable
        }
        Ok(true) => {
            println!("removed {name}");
            println!("  it takes effect at the next daemon start");
            hot_disable_best_effort(name).await;  // never returns an Err;
                                                     // only prints
            Ok(())
        }
    }
}
```

```rust
/// Best-effort: tells an already-running daemon to stop serving `name` now,
/// rather than leaving a healthy module to keep answering until it next
/// happens to restart (which Part A then catches as `PackageChanged`, but
/// only for a module that DOES eventually restart). Never starts a daemon
/// (`attach_only`, not `connect` — a fresh ephemeral daemon here could run
/// the very package just removed, Codex round 1 High 2) and never turns
/// into an `Err` the caller has to handle — `cmd_uninstall`'s `Ok(())` is
/// already final by the time this runs.
async fn hot_disable_best_effort(name: &str) {
    let Some(ep) = attach_only().await else {
        println!(
            "  no reachable daemon right now; a module of it running elsewhere will report \
             `package_changed` after its own next restart"
        );
        return;
    };
    // POST .../stop (v4), NOT the PATCH used by `agent24 os disable` — that
    // one persists `enabled:false` into `os.json`, which is actively wrong
    // here (see Part B's v4 note: it would trip `unknown_disabled` on the
    // daemon's next start and degrade every OTHER module too). This is a
    // one-shot "stop it now", nothing written for next time.
    let sent = bearer(&ep, client().post(format!("{}/api/v1/os/{name}/stop", ep.base)))
        .timeout(Duration::from_secs(5))
        .send()
        .await;
    match sent {
        // 2xx covers `HotStop::{Stopping, Already, NotRunning}` alike
        // (`os_routes.rs`) — the body alone does not say which, so the
        // message must not claim "it was running and I stopped it"; that
        // is exactly the overclaim Codex round 2's Medium finding caught.
        Ok(res) if res.status().is_success() => println!(
            "  told the running daemon to stop serving it — `agent24 os list` shows whether a \
             running module was actually there to stop"
        ),
        // `disable_pending` (503) / `stop_failed` (500): the stop was
        // handed off before either of these is returned, and (v4) nothing
        // was persisted either way — this module will not restart on its
        // own from a config change, but a module that later crashes and
        // gets manually restarted (or was never told to stop because the
        // hand-off itself failed) can still reach `PackageChanged` through
        // Part A same as ever. Relay the daemon's own message.
        Ok(res) => {
            let body: serde_json::Value = res.json().await.unwrap_or_default();
            let msg = body["error"]["message"].as_str().unwrap_or("(no detail)");
            println!("  the daemon could not fully confirm the stop: {msg}");
        }
        // Genuinely ambiguous: the request may never have reached the
        // daemon, or it may have applied the change and the response was
        // lost. Neither "not stopped" nor "will report package_changed"
        // can be asserted here — say so plainly instead of guessing.
        Err(e) => println!(
            "  could not confirm the daemon received this ({e}) — if it did not, a module of \
             it still running there will report `package_changed` after its own next restart; \
             if it did, that module has already been told to stop"
        ),
    }
}
```

```rust
/// Attaches to an already-running, healthy daemon; NEVER starts one
/// (unlike `connect`, which falls back to `spawn_daemon` — wrong for a
/// best-effort step that must not bring up the package it might be
/// trying to get rid of). `None` if no live daemon is discoverable or
/// healthy.
async fn attach_only() -> Option<Endpoint> {
    let state = state_file::read_live()?;
    let base = format!("http://127.0.0.1:{}", state.port);
    health_ok(&base, &state.token)
        .await
        .then_some(Endpoint { base, token: state.token, child: None })
}
```

这个顺序让第 A 节和第 B 节成为**两条互不依赖的安全网**，而不是一条链条上互相牵连的两步：文件删除失败,热停从未发生,状态和今天完全一样（`os_local` 已有的失败处理不变）；文件删除成功、热停也成功,模块立刻不再被路由；文件删除成功、热停失败或 daemon 不可达,操作者从 CLI 输出就能看到「没停成」，而且不用等——这个「继续跑着的模块」在它自己下一次重启时,已经被第 A 节的 `recheck` 兜底,不会崩溃循环，只会报 `PackageChanged`。**不存在「daemon 已经改了状态、文件却没删掉」这种半途状态**，因为热停永远在文件删除已经确认成功之后才发生。

**名字歧义,不是这次新引入的（v2 新增，回应 Codex Medium 4 的部分内容）**：`PATCH /api/v1/os/{name}` 只按名字定位,如果 CLI 和 daemon 各自解析出的 `A24_OS_PACKAGES` 根不同,名字相同并不保证是同一个包——这和今天已有的 `agent24 os disable <name>` 面对的是完全同一个歧义,不是本设计引入的新问题,不在这次解决。热停请求发出之前,`name` 已经在 `os_local` 里做过合法性校验（`is_valid_module_name`）并且**已经**被确认是这次真的删除的那个名字（不是「先发请求、后校验」——v1 的顺序被 v2 的整体反转一并修正）。

**CLI 提示文字**（`install.rs::uninstall` 调用点）：现有的
```
"  (a module of it running now keeps running, but cannot be restarted if it exits)"
```
删除，改成上面三种（已停 / daemon 拒绝或够不到 / 没有可达 daemon）分支各自打印的准确文字——不再有一句笼统覆盖所有情形的话。

## 判据（带正对照；v2 改写回应第 1 轮 High 1/Medium 3；v3 改写第 5/8/9/10 条并新增 11–14；v4 改写第 8/10/15 条、新增 16/17；v5 改写第 15/17 条、新增 18/19，回应第 4 轮「`stop_now_os` 漏了 `last_look` 复核」与相关 Low）

1. **重启前核对，退避期间发生的变更也要抓到**（钉住检查时机在睡眠之后，回应 High 1）：一个模块崩溃一次进入 backoff；在**退避睡眠期间**把 `package_dir` 整个删掉；断言退避结束后状态直接是 `PackageChanged`，而不是「又进了一次 Backoff（`policy.consecutive_failures()` 增加）」——用状态转换序列本身作证据，不依赖对 spawn 内部打点（回应 Medium 3：目录被删的情形下,就算完全没有这次改动,`launch::resolve` 也会在 spawn 之前失败、不会有任何子进程留下痕迹,「子进程有没有起来」这件事本身分不出「检查生效」和「检查没生效但反正也会失败」这两种情况；能分辨的是`policy`的失败计数有没有继续往上走）。
2. **原地替换：证明真的没有发起第二次握手**（这里 spawn 会成功、能装出「起来了」的假象,所以要用真正能证明「有没有跑」的信号）：包目录本身仍然存在，只改写 `domain-os.yml`（不同摘要）；在退避睡眠期间做这个改写；用现有的「子进程启动时记一笔」夹具模式，断言标记文件的写入次数在改写前后**没有增加**——如果检查没生效,子进程会被 spawn 起来并至少写一次标记（随后才在握手时被拒），计数增加就是检查失效的信号。
3. **负对照（不能误伤健康重启）**：包原样不动，模块自身崩溃重启；断言仍然正常重启并恢复 `Running`。
4. **不影响首次启动**：`mount()` 首次 spawn（`attempt == 1`）不经过这次检查——检查只写在退避之后这一件事本身保证了这条边界，不需要专门测试，但 review 时要点出。
5. **`agent24 os list` 的诊断文本**：`os_routes.rs` 新增分支的字符串化断言——同时包含 `reason` 原文和「重启 daemon」这条唯一真实有效的恢复办法（不再断言任何「disable/enable」字样）；连带断言第 C 节修好之后 `agent24 daemon stop && agent24 daemon start` 这句具体命令确实可行（见判据 15）。
6. **HTTP 层不变**（回归判据）：`PackageChanged` 状态下打到该模块的请求仍然是既有的 `503 module_stopping`。
7. **`recheck` 的目录级校验**（回应 High 4）：把 `package_dir` 换成指向一个内容完全相同的目录的符号链接（清单字节不变、摘要当然也不变）；断言 `recheck` 仍然返回 `Err`（符号链接本身不可信,不能因为「里面的清单看起来没变」就放行）——对照 `discovery::scan` 已有的同类测试。
8. **uninstall 热停（daemon 可达、模块确实在跑）**：预先起一个 daemon、挂载一个模块、`agent24 os uninstall` 该模块；断言（a）`POST .../stop` 被调用、该模块的 supervisor 停止/退出注册表，（b）包目录被删除，（c）CLI 输出是「已通知 daemon 停止服务」这一类不过度断言的措辞，不是「已确认停止了正在运行的服务」，（d）**`os.json` 里没有出现这个包名的新条目**（v4 新增断言，这正是第 3 轮 High 钉住的那个 tombstone）。
9. **uninstall 热停：`HotStop::NotRunning` 不能被说成「停掉了在跑的」**（回应「2xx 不等于真的在跑」）：卸载一个当前**没有**被挂载/没有运行中 supervisor 的包名（daemon 可达，`POST .../stop` 走 `HotStop::NotRunning` 分支，2xx）；断言 CLI 的输出不含任何暗示「它本来在跑、被我停掉了」的措辞——2xx 时的文案对 `Stopping`/`Already`/`NotRunning` 三种服务端状态必须是同一句、不过度承诺的话。
10. **uninstall 热停三条负对照**：
    - **daemon 不可达**：断言 `attach_only()` 不启动任何新进程、`uninstall` 依然成功、CLI 打印「没有可达 daemon」这句话——不是「等 15 秒超时才继续」（判据 13 给出更强的「确实没起进程」的证明方式）。
    - **daemon 可达但该模块 `disable_pending`/`stop_failed`**：断言 CLI 转述的是 daemon 返回的 `error.message` 原文，且明确不包含「它会在下次重启时报 package_changed」这句话（这两种情形下 supervisor 已经被从注册表撤走，不会再有下一次重启）；`uninstall` 本身依然成功；断言 `os.json` 依旧没有这个包名的新条目（`stop_now_os` 从不写配置,这两个错误码本身就来自「握手/停止本身失败」而不是「配置写入失败」）。
    - **文件删除本身失败**（例如目标被外部占用导致 rename 失败）：断言全程**不发出**热停请求——`cmd_uninstall` 在 `install::uninstall` 返回 `Err` 时直接返回，`hot_disable_best_effort` 不会被调用到。
11. **`Ok(false)`（本来就没装）不发热停请求**：卸载一个从未安装过的名字；断言 `install::uninstall` 返回 `Ok(false)` 走到 CLI 的「nothing to remove」分支之后，全程没有任何 `POST .../stop` 请求发出——这是幂等卸载，没有「新发生的删除」可供热停响应。
12. **传输错误的措辞不作假设**：让 `POST .../stop` 请求本身超时或连接被重置（不是拿到一个 HTTP 错误状态码,是连请求都没走完）；断言 CLI 的输出用的是「无法确认」这类中立措辞，既不断言「没停」也不断言「会在下次重启时报 package_changed」。
13. **确实没有起新进程,不是靠看状态文件猜的**：`spawn_daemon(true)` 起临时 daemon 时故意不写发现用的状态文件，短命子进程也可能在按 pid 扫描之前就已经退出——单纯检查 `run/` 目录或扫描进程列表不能可靠地证明「一次都没被启动过」。改用一个假的 `AGENT24D_BIN`（测试环境变量指向一个 shell 脚本），脚本在被调用的瞬间就往一个测试专属文件追加一行、再退出；`daemon 不可达` 场景下跑完 `uninstall` 后断言这个标记文件不存在（不是「进程列表里没有」，是「这个可执行文件从未被 exec 过」）。
14. **名字先本地校验再发热停请求**：一个不合法的模块名（未通过 `is_valid_module_name`）执行 `uninstall`；`install::uninstall` 在任何文件系统改动之前就已经拒绝了它（`install.rs` 现有校验），`cmd_uninstall` 走 `Err` 分支直接返回，断言全程没有任何网络请求发出——`hot_disable_best_effort` 只在 `Ok(true)` 分支里才可能被调用，不合法名字连这个分支都进不去。
15. **`daemon stop && daemon start` 复合命令确实可靠（回应第 C 节；v4 改用真正的 oracle）**：持锁方必须是**真正独立的子进程**（不是同一个测试进程内部模拟——`fs2` 的 `try_lock_exclusive` 是按进程通告的,同进程内两次尝试拿同一把锁不能如实反映跨进程行为,v5 回应第 4 轮 Low 明确这一点）：起一个真正的假 daemon 子进程持有 `daemon.singleton.lock`、刻意让健康检查先失效但仍然多持有锁一小段时间；跑 `agent24 daemon stop`，断言它返回时 `try_acquire_singleton()` 已经能拿到锁（不是「health 探测失败」就提前判定——这正是要证明 v3 的 oracle 会提前误报、v4 的不会）；紧接着跑 `agent24 daemon start`，断言它走的是正常 spawn 路径成功（不是撞锁之后的 30 次轮询兜底分支）。负对照：把 `STOP_CONFIRM_BUDGET` 临时调到一个不够长的值,对一个刻意长时间持有锁不放的假 daemon 子进程跑 `stop`，断言它诚实地返回「没能在预期内停止」的错误，而不是谎称成功。
16. **uninstall 热停之后重启,不能拖垮其它模块**（v4 新增，回应第 3 轮 High「tombstone」——这是本设计最关键的一条回归判据）：默认 `Enabled` 策略下起一个 daemon、挂载两个模块 A、B；`agent24 os uninstall A`（daemon 可达，走完整的「文件删除 + 热停」）；重启 daemon；断言 **B 仍然正常挂载**、daemon 的注册表校验没有报 `registry_error`——变异验证：把 `stop_now_os` 换回旧的 `PATCH`（写 `enabled:false`）,这条测试必须变红。
17. **`stop_now_os` 路由本身**：对一个不存在的模块名 `POST .../stop`，断言 `404 not_found`（v5 更正：和 `patch_os` 实际用的错误码字面一致，不是编一个新的 `unknown_module`）；对一个正常运行的模块，断言响应体是完整的 `DomainOsList` 且这次调用之后 `os.json` 文件的 mtime/内容**没有变化**（直接证明这条路由完全不碰配置文件,不是靠「猜结果对不对」间接证明）。
18. **`stop_now_os` 真的做了 `last_look` 复核，不是一次性读一眼**（v5 新增，回应第 4 轮 Medium）：构造一个「`settle()` 采样时还是 `Stopping`、复核那一刻已经变成 `StopFailed`」的时序（复用 `patch_os` 已有测试驱动 `last_look` 的同一套手法）；断言 `stop_now_os` 返回的是 `500 stop_failed`,不是把 `settle()` 那一次性采样直接当结果、误报 2xx。
19. **鉴权覆盖新路由**（v5 新增，回应第 4 轮 Low）：既有的「注册表鉴权」测试今天只覆盖 `GET`/`PATCH` `/api/v1/os/*`，加上 `POST /api/v1/os/{name}/stop`——鉴权是路由组装之后全局包一层，新路由按结构不会漏,但要有一条测试把这句话钉死,不是靠读代码相信。

## 不改的东西

- `RestartPolicy` 的退避/熔断算法本身——`PackageChanged` 完全绕开它。
- `GaveUp` 的语义和收尾时序——只是被 `PackageChanged` 照抄了一份。
- `install.rs::uninstall` 的文件删除主干（staging-then-remove）——热停是它之后新加的一步，不改它本身的原子性处理，也不改变它的返回值语义。
- `os_local()` / `cmd_os()`「uninstall 不要求 daemon 在跑」这条既有契约——`attach_only()` 从不启动 daemon，热停失败绝不影响 `uninstall` 本身的成败。
- `agent24 os disable`/`enable` 现有的按名字定位的歧义（CLI 和 daemon 各自解析出的包根可能不同）——新的 `POST .../stop` 同样只按名字定位、同一类歧义，不在这次新增或修复。
- `os.json` 里可能早就存在的、针对这个包名的旧配置条目（例如以前手动 `disable` 过它）——v4 只保证「这次新加的热停步骤不会新制造一个 tombstone」，不负责清掉卸载前就已经写在 `os.json` 里的旧条目；这条本身是既有的、和 FU-61 无关的残余。
- `agent24 daemon stop && agent24 daemon start` 会让一个由 `agent24 service install`（launchd）托管的 daemon 脱离托管——这不是 FU-61 造成的（`restart_required` 提示今天就已经建议同一句话），第 C 节只让这句命令本身可靠，不解决「托管感知」，记为 `followups.md` 的 **FU-68**，这次不做。
- 快照方案——已被用户裁决为不做。
- `agent24-os-proto` 对 `agent24-os-packages` 的依赖边——本设计特意不新增，靠 `PackageCheck` 闭包维持现状。
- `ERR-1` 要做的「所有连不上模块的响应统一 `hint` 字段」——FU-61 只把 `Status::PackageChanged` 这一个变体的文字写清楚。
- 「热重挂载」（在运行中的 daemon 里为同一个模块重新造一个 supervisor，而不必整个重启）——v1 曾经暗示这条路径存在（「disable 再 enable」），v2 已确认它不存在；要不要做是一个新的、更大的状态机设计,这次不做,也不假装它已经能用。
- `daemon start` 撞锁之后的轮询兜底逻辑——第 C 节只改 `stop`，`start` 面对一个已确认释放的锁时,今天已有的正常路径就够用,不用碰。
- SHUT-1 系列自己的排空/宽限超时与告警——`STOP_CONFIRM_BUDGET` 只是 CLI 这一端「要不要继续等」的上限，不是 daemon 内部停机行为的一部分，两者互不影响。

## v1 → v2 改动（Codex 第 1 轮：0 Critical、6 High、5 Medium、1 Low，全部采纳）

- High 1：检查时机从「退避睡眠之前」搬到「睡眠之后、下一次 spawn 之前」，并写明残余的 TOCTOU 窗口不做承诺。
- High 2：`connect()` 会在无健康 daemon 时启动一个临时 daemon——引入 `attach_only()`，热停步骤永不起 daemon。
- High 3：热停（写 `os.json` + 停止）与文件删除的顺序整个反转——文件删除永远先做且唯一决定成败，热停挪到之后、纯尽力而为。
- High 4：`recheck` 补上包目录本身的符号链接/类型校验（`read_package` 只查清单文件,不查目录本身）。
- High 5：删除「`os disable` 再 `enable`」这条实际不起作用的恢复建议，改为唯一真实有效的「重启 daemon」；连带把「`daemon status` 可见」的说法收窄成「`os list`」（吸收原 Low 1）。
- High 6：热停不再只看「连上了没有」，改成按 `PATCH` 真实响应状态打印三选一的准确文字。
- Medium 1：`PackageCheck` 的阻塞文件 I/O 通过 `spawn_blocking` 调用，不直接跑在 async 任务上。
- Medium 2：新增「范围收窄」一节，明确本设计只检测清单文件本身的变化，不检测包内其它文件。
- Medium 3：判据 1/2 改写——目录删除的情形改用状态转换序列作证据，替换的情形改用「子进程标记文件是否新增写入」而不是简单的「有没有起来」。
- Medium 4：热停请求的名字先经过 `os_local` 本地校验才发出（v2 顺序反转的自然结果）；跨 daemon/CLI 包根的名字歧义记为既有问题、不在这次解决。
- Medium 5：判据补齐 daemon 不可达 / `disable_pending`/`stop_failed` / 文件删除先失败三条负对照，以及名字先校验后发请求这条。

## v2 → v3 改动（Codex 第 2 轮：0 Critical、1 条 High（部分残留）、3 条 Medium、2 条 Low，全部采纳；其余 v2 修法全部核实为 RESOLVED）

- High 5（残留部分）：`agent24 daemon stop && agent24 daemon start` 本身不可靠（`stop` 不等进程退出就返回成功,`start` 撞锁后只轮询、不重试 spawn）——新增**第 C 节**，让 `daemon stop` 等到健康检查确认失败再返回,使这条复合命令（连带既有的 `restart_required` 提示）变得可靠。
- Medium：`os_local()`/`cmd_os()` 的真实返回类型（`Option<Result<(), String>>` / `Result<(), String>`）装不下 v2 pseudocode 想要的「按 `bool` 决定要不要热停」——把 `Uninstall` 从 `os_local()` 的通用分派里摘出来，单独一个 `cmd_uninstall`，直接拿 `install::uninstall` 的 `Result<bool, InstallError>`，`Ok(false)`（本来没装）不发热停请求。
- Medium：PATCH 结果的三选一文案仍不准确——2xx 覆盖 `HotStop::{Stopping, Already, NotRunning}` 三种情形，不能说成「停掉了正在跑的」；`disable_pending`/`stop_failed` 这两种情形下 supervisor 已经被撤出注册表、不会再有下一次重启，v2 「它会在下次重启时报 package_changed」这句话对这两种情形是错的，改成转述 daemon 自己的 `error.message`；传输错误的措辞改成明确的「无法确认」，不再断言任何一边。
- Medium（Codex 第 2 轮「STILL BROKEN」）：判据矩阵补齐 `HotStop::NotRunning` 不过度断言、传输错误措辞中立、幂等 `Ok(false)` 不发请求、用假 `AGENT24D_BIN` 可靠证明「一次都没起进程」这四条（判据 9、11、12、13）。
- Low：`attach_only()` 的说法从「never blocks」改为如实的「不会走 15 秒起daemon那条路，但健康探测 + `PATCH` 各自的超时仍然是要等的」（体现在文档不再使用「never blocks」这个措辞,已随 Part B 重写一并去除）。
- Low：`recheck` 的 rustdoc 不再说「safe to restart against」，改成「manifest-consistent under this check's scope」,和「范围收窄」一节的措辞对齐。

## v3 → v4 改动（Codex 第 3 轮：0 Critical、2 条 High（1 条全新、1 条 STILL BROKEN）、1 条 Medium（全新）、1 条 Low，全部采纳）

- High（全新，最严重）：v3 的热停复用 `PATCH /api/v1/os/{name}`，会把 `enabled:false` 持久写进 `os.json`；对一个即将从磁盘消失的包名，这条持久化在下次 daemon 启动时被 `unknown_disabled()` 当成危险的拼写错误,把**整个注册表**判定非法、连带降级所有其它模块——不是这一个包的问题。改法：新增一条不写配置、只管「现在停」的 `POST /api/v1/os/{name}/stop` 路由，复用 `patch_os` 已经拆好的 `hand_off`/`settle` 内部函数,跳过配置写入这一步。判据新增 16（回归判据，变异验证换回旧 PATCH 必须变红）、17（新路由本身）。
- High（STILL BROKEN，回应第 2 轮同一条）：v3 用 `health_ok()` 变假当作「已经停了」的证据，但这只证明 Axum 的 accept 循环关了，不证明进程已经退出、单例锁已经释放——而后者才是 `daemon start` 真正需要的条件。改法：直接轮询 `agent24_protocol::state_file::try_acquire_singleton()`（`daemon start` 会用的同一把锁），拿到立刻释放。判据 15 改用这个新 oracle 做区分测试。
- Medium（全新）：`agent24 daemon stop && agent24 daemon start` 会让一个 launchd 托管的 daemon 脱离托管（新进程走手动 spawn，不经过 `launchctl`）——这是既有问题（`restart_required` 提示今天就已经受它影响），不在 FU-61 里解决，记为新的 `followups.md` **FU-68**。
- Low：`STOP_CONFIRM_BUDGET` 的取值理由改对——`lifecycle.rs` 的真实上限约 15.7s（排空 ≤10s + 宽限 ≤5s + 落盘/收尾余量），模块是用 `JoinSet` **并发**停止的，不是 v3 说的「可能顺序停止」；新 oracle 是本地文件锁探测，没有 v3 那种「`health_ok` 自带 3 秒超时叠加到总预算」的问题。

## v4 → v5 改动（Codex 第 4 轮：0 Critical、0 High（两条第 3 轮 High 均核实为 RESOLVED）、1 条 Medium（全新）、4 条 Low，全部采纳）

- Medium（全新）：`stop_now_os` 只调了一次 `settle()` 就直接决定响应,漏掉了 `patch_os` 本来就有的 `last_look()` 复核——一个采样时是 `Stopping`、复核那一刻已经变成 `StopFailed` 的运行，会被误报成 2xx。改法：`stop_now_os` 换成和 `patch_os` 完全一样的 `hand_off` → `settle` → `last_look` 三步,不是自己另起一套一次性读法。新增判据 18 专门钉住这条复核确实被调用。
- Low：`stop_now_os` 的「未知名字」错误码从编造的 `unknown_module` 改成和 `patch_os` 字面一致的既有码 `not_found`（判据 17 同步改写）。
- Low：新增判据 19，把 `POST .../stop` 加进既有的「注册表鉴权」测试——鉴权是路由组装后全局包一层,结构上不会漏,但要有测试钉死而不是靠读代码相信。
- Low：Part A 新增一段,如实记录 `check_package` 和 `stop_now_os` 之间一条很窄的时序竞态（可能瞬间发布一次随后被覆盖的 `PackageChanged`,不产生任何额外 spawn、最终状态仍然正确）——「热停后的模块不会再触发 `PackageChanged`」这句话加上「稳定态下」的限定,不再是无条件的「从不」。
- Low：判据 15 明确持锁方必须是真正独立的子进程,不能在同一个测试进程里模拟——`fs2` 的排他锁是按进程通告的,同进程内的两次尝试证明不了跨进程行为。
- （核实为 RESOLVED，不算改动）第 3 轮两条 High——`stop_now_os` 不写 `os.json`（`hand_off`/`Supervisors::disable` 只改内存态,`render`只读不写）、新 oracle 确实是 `daemon start` 会用的同一把跨进程排他锁——均已核实成立，v5 未改动这两处的机制本身。
