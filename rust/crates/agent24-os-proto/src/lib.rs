//! ME-3b — the kernel↔domain-OS protocol.
//!
//! Both directions live here. **Inbound** is [`proxy`]: the kernel forwards its
//! own `/api/v1/<ns>/*` to the module, stripping its credentials on the way in
//! and the module's echoes on the way out (§2). **Outbound** is the callback
//! channel the module talks back on — [`frame`] reads one line of it,
//! [`version`] settles which protocol version the two agreed on, and
//! [`initialize`] is the handshake that uses both. [`launch`] starts the process
//! and [`supervise`] decides what happens when it stops.
//!
//! # Why the pure parts are separate modules
//!
//! [`version::negotiate`], and the header rules in [`proxy`], are pure functions
//! — of two version ranges, and of a header map. Nothing about either needs a
//! connection, a subprocess, or a byte stream, so both can be settled, and
//! disagreed with, before any of those exist. That ordering is deliberate: the
//! negotiation table is copied from the SPEC rather than invented alongside the
//! implementation, which makes it the one part of ME-3b whose expected answers
//! are not decided by whoever writes the code.

pub mod frame;
pub mod initialize;
pub mod launch;
pub mod proxy;
pub mod supervise;
pub mod version;
