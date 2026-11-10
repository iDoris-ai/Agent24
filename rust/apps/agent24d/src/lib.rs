//! Injectable managed iDoris connection lifecycle for the daemon.
#[cfg(all(feature = "test-support", not(debug_assertions)))]
compile_error!("test-support must not be enabled in release builds");

pub mod managed_idoris;

pub mod idoris_host;
