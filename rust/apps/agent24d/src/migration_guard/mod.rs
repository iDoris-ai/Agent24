//! CORE-5c-1 migration-guard building block: space budget (manifest and
//! guard log follow in 5c-2). Internal API only; not wired into startup (the
//! coordinator is still `ENABLED=false`). Lives in agent24d beside the
//! coordinator because every consumer (5d snapshot, 5e/5f restore and dock)
//! is a startup-path concern of the daemon, and it needs no store/memory
//! internals.

pub mod space;
