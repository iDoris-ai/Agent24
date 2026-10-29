//! Minimal installation subprocess adapter; no launcher or service behavior.
fn main() -> std::process::ExitCode {
    agent24_idoris::install::run_install_child()
}
