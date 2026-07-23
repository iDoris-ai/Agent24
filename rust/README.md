# rust/ — Agent24 Rust core (placeholder)

任务 B1 起在此建立 Cargo workspace（见 `docs/ADR-026-rust-core-polyglot.md` §1 与 `docs/specs/TASKS.md` M-B）：

```
rust/
├── crates/    # agent24-core / -protocol / -agent / -models / -scheduler /
│              # -policy / -tools / -store / -memory / -mcp
└── apps/
    ├── agent24d/      # daemon（REST + WS，动态端口 + token）
    └── agent24-cli/   # CLI + `agent24 tui`
```

在 B1 合入前本目录仅有此 README；CI 的 rust job 以目录内是否存在 `Cargo.toml` 为启用开关。
