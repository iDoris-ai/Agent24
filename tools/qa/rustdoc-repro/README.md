# rustdoc E0463 最小复现工装（test-only）

不依赖 Agent24 源码、无外部 crate、全部 `--offline`，target 在调用方给定的临时目录下（不是 Agent24 的 `rust/target`）。

| 文件 | 作用 |
|---|---|
| `make-fixture.sh <ROOT>` | 生成共享 proc-macro `fake_async_trait`（模拟 async-trait：两个 workspace 看到的是同一个 PackageId）+ 两个 source 路径不同、内容相同的 workspace `ws_a`/`ws_b`（`fx_core → fx_store → fx_agent`，`fx_agent` 带 1 个 doctest 和一个可调时长的慢集成测试） |
| `run-experiments.sh <WORK> [toolchain]` | E1–E13 正/反控制；所有 cargo 都用 `env -i` 白名单 + 假 HOME/CARGO_HOME/XDG/TMP/CFFIXED_USER_HOME 运行 |
| `target-window-watch.sh <TARGET> <LOG> [crate...]` | 只读监视器：原样跑 cargo test 时，记录指定 crate 产物和 fingerprint 的 inode/mtime/size 变化（macOS/Linux 都能用） |

```bash
bash tools/qa/rustdoc-repro/run-experiments.sh /tmp/rdrepro 1.95.0
```

结果与解读见 `docs/agent/cloud-wave37/a24-rustdoc-20261015-104002.md`。
