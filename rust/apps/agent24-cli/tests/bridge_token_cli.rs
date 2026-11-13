//! `agent24 dev bridge-token` refuses before it looks for a daemon when
//! `A24_DEV_TOOL` is not `1`. Deleting that check in `cmd_bridge_token`
//! turns this red: the error becomes "找不到运行中的 agent24d".

#![allow(clippy::unwrap_used)]

#[path = "support/signature_command.rs"]
mod signature_command;

use std::process::Output;

fn cli(args: &[&str]) -> Output {
    let home = tempfile::Builder::new()
        .prefix("a24bridge")
        .tempdir_in("/tmp")
        .unwrap();
    let mut cmd = signature_command::clear_command(env!("CARGO_BIN_EXE_agent24"));
    cmd.env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn bridge_token_without_dev_tool_does_not_ask_the_daemon() {
    for args in [
        &["dev", "bridge-token"][..],
        &["dev", "bridge-token", "--revoke"][..],
    ] {
        let out = cli(args);
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert!(!out.status.success(), "{stderr}");
        assert!(
            stderr.contains("微信桥令牌只在开发时签发"),
            "{args:?} stderr={stderr}"
        );
        assert!(
            !stderr.contains("找不到运行中的 agent24d"),
            "{args:?} stderr={stderr}"
        );
        assert!(
            !stderr.contains("Waiting for approval"),
            "{args:?} stderr={stderr}"
        );
    }
}
