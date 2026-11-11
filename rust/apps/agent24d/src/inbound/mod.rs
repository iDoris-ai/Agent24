//! Inbound HTTP surface (X11-2f) and admission (X11-2b).
//!
//! `admit` is the pure policy hash and the §3.3 table. `routes` serves HTTP.
//! X11-2c wires `admit` into the event path. `policy_version` is already
//! called from `routes`. Items that only the table uses carry
//! `cfg_attr(not(test), expect(dead_code))`: the bin does not call them yet,
//! and the admit tests do, so a module-level expect is unfulfilled there.

mod admit;
mod routes;

pub(crate) use routes::{get_allowlist, get_events, get_policy, put_allowlist, put_policy};
