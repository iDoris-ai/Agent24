//! ME-3b — the kernel↔domain-OS protocol.
//!
//! Today this crate holds two: the version negotiation the `initialize`
//! handshake performs ([`version`]), and the framing that reads one line off the
//! callback channel ([`frame`]). The `initialize` wire shape (ME-3b-2b) — the
//! consumer of both — lands here next.
//!
//! # Why negotiation is here and not inside the handshake
//!
//! It is a pure function of two version ranges. Nothing about it needs a
//! connection, a subprocess, or a byte stream — so it can be settled, and
//! disagreed with, before any of those exist. That ordering is deliberate: its
//! test table is copied from the SPEC rather than invented alongside the
//! implementation, which makes it the one part of ME-3b whose expected answers
//! are not decided by whoever writes the code.

pub mod drain;
pub mod frame;
pub mod initialize;
pub mod launch;
pub mod supervise;
pub mod version;
