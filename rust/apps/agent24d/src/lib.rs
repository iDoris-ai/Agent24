//! Daemon library target. Managed iDoris lives in `agent24-idoris`.
#[cfg(all(feature = "test-support", not(debug_assertions)))]
compile_error!("test-support must not be enabled in release builds");
