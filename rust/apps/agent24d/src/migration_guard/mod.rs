//! CORE-5c migration-guard building blocks: space budget, snapshot manifest,
//! durable guard log. Internal API only; not wired into startup (the
//! coordinator is still `ENABLED=false`). Lives in agent24d beside the
//! coordinator because every consumer (5d snapshot, 5e/5f restore and dock)
//! is a startup-path concern of the daemon, and it needs no store/memory
//! internals.

pub mod atomic;
pub mod guard_log;
pub mod manifest;
pub mod space;
