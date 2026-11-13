//! Environment for real-binary tests; fixtures carry valid ad-hoc signatures.
#![allow(dead_code)]
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command.env("A24_TEST_ACCEPT_ADHOC_SIGNATURES", "1");
    command
}
pub fn clear_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut command = command(program);
    command
        .env_clear()
        .env("A24_TEST_ACCEPT_ADHOC_SIGNATURES", "1");
    command
}
