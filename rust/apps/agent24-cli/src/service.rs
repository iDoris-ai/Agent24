//! 24/7 unattended operation (M-F): install `agent24d` as a macOS LaunchAgent.
//!
//! Deliberately does NOT implement its own supervisor. launchd already provides
//! everything the "24/7" requirement needs, and does it better than a hand-rolled
//! parent process could:
//! - `RunAtLoad` → starts at login without anyone typing a command;
//! - `KeepAlive{SuccessfulExit:false}` → restarts a CRASH, but honours a clean
//!   `agent24 daemon stop` (exit 0) instead of resurrecting it against the user's
//!   wishes — the distinction a naive "always restart" supervisor gets wrong;
//! - `ThrottleInterval` → a crash loop backs off instead of hammering the CPU;
//! - `StandardOut/ErrorPath` → crash output survives for diagnosis.
//!
//! A supervisor of our own would also die with its own bugs; launchd is started
//! by the OS and cannot.

use std::path::{Path, PathBuf};

/// launchd job label; matches the desktop bundle's appId.
pub const LABEL: &str = "ai.auraai.agent24";

/// `~/Library/LaunchAgents/ai.auraai.agent24.plist`
pub fn plist_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| plist_path_in(Path::new(&h)))
}

/// Pure form, so the layout is testable without mutating the process env.
pub fn plist_path_in(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

/// Where launchd writes the daemon's stdout/stderr.
pub fn log_dir() -> Option<PathBuf> {
    agent24_protocol::state_file::state_dir().map(|d| d.join("logs"))
}

/// Escape text destined for an XML text node. Paths can legitimately contain
/// `&` or `<` (e.g. a folder named "R&D"), which would otherwise produce a plist
/// launchd silently refuses to load.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Render the LaunchAgent plist.
///
/// `ThrottleInterval` is 10s: launchd's own minimum, and enough that a daemon
/// crash-looping on a bad config burns ~0.1 CPU instead of a core.
pub fn render_plist(
    exec: &Path,
    out_log: &Path,
    err_log: &Path,
    env: &[(String, String)],
) -> String {
    let exec = xml_escape(&exec.to_string_lossy());
    let out_log = xml_escape(&out_log.to_string_lossy());
    let err_log = xml_escape(&err_log.to_string_lossy());
    let env_block = if env.is_empty() {
        String::new()
    } else {
        let mut b = String::from("    <key>EnvironmentVariables</key>\n    <dict>\n");
        for (k, v) in env {
            b.push_str(&format!(
                "        <key>{}</key>\n        <string>{}</string>\n",
                xml_escape(k),
                xml_escape(v)
            ));
        }
        b.push_str("    </dict>\n");
        b
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exec}</string>
        <string>serve</string>
        <string>--port</string>
        <string>0</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>ProcessType</key>
    <string>Background</string>
{env_block}    <key>StandardOutPath</key>
    <string>{out_log}</string>
    <key>StandardErrorPath</key>
    <string>{err_log}</string>
</dict>
</plist>
"#
    )
}

/// Config the daemon reads from the environment. launchd gives a LaunchAgent
/// NONE of the login shell's environment, so without capturing these the 24/7
/// daemon silently behaves differently from a manually started one.
pub const PASSTHROUGH_VARS: [&str; 10] = [
    "OMLX_URL",
    "OMLX_API_KEY",
    "DEFAULT_MODEL",
    "A24_GUARDIAN",
    "A24_GUARDIAN_ALWAYS_REVIEW",
    "A24_APPROVAL_TIMEOUT_SECS",
    "A24_SCHEDULER_TICK_SECS",
    // ME-3a. Missing it meant `agent24 os install` wrote to the override
    // directory while the launchd-started daemon kept scanning
    // `~/.agent24/packages` — and the CLI still printed "it takes effect at the
    // next daemon start".
    "A24_OS_PACKAGES",
    // SHUT-1b: the modules' shutdown budgets. A service that should run with
    // raised ones gets them only through here — and only after `agent24 service
    // install` is re-run, since this is a snapshot.
    "A24_MODULE_DRAIN_MS",
    "A24_MODULE_STOP_GRACE_MS",
];

/// Snapshot the environment the daemon should run with.
///
/// PATH matters most: launchd's default is only `/usr/bin:/bin:/usr/sbin:/sbin`,
/// so a `shell_exec` tool call for `node`/`git`/`python3` would fail under 24/7
/// while working when started by hand — the worst kind of bug to diagnose.
/// This is a SNAPSHOT taken at install time; re-run install to refresh it.
pub fn capture_env() -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        out.push(("PATH".to_owned(), path.to_string_lossy().into_owned()));
    }
    for key in PASSTHROUGH_VARS {
        if let Some(val) = std::env::var_os(key) {
            out.push((key.to_owned(), val.to_string_lossy().into_owned()));
        }
    }
    out
}

fn launchctl(args: &[&str]) -> Result<std::process::Output, String> {
    std::process::Command::new("launchctl")
        .args(args)
        .output()
        .map_err(|e| format!("running launchctl {}: {e}", args.join(" ")))
}

fn uid() -> u32 {
    // SAFETY-free: getuid via `id -u` avoids a libc dependency for one number.
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(501)
}

/// `launchctl bootout` the job if loaded. Not-loaded is success, not an error.
fn bootout_if_loaded() {
    let target = format!("gui/{}/{LABEL}", uid());
    let _ = launchctl(&["bootout", &target]);
}

/// Install (or reinstall) the LaunchAgent and start it.
pub fn install(exec: &Path) -> Result<(PathBuf, Vec<String>), String> {
    if !exec.exists() {
        return Err(format!(
            "agent24d not found at {} — build it (cargo build --release -p agent24d) \
             or set AGENT24D_BIN",
            exec.display()
        ));
    }
    let plist = plist_path().ok_or("HOME not set")?;
    let logs = log_dir().ok_or("HOME not set")?;
    std::fs::create_dir_all(&logs).map_err(|e| format!("creating {}: {e}", logs.display()))?;
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    // Reinstall must unload the old job first, or launchd keeps running the
    // previous ProgramArguments and the new plist looks like it did nothing.
    bootout_if_loaded();

    let env = capture_env();
    let body = render_plist(
        exec,
        &logs.join("agent24d.out.log"),
        &logs.join("agent24d.err.log"),
        &env,
    );
    std::fs::write(&plist, body).map_err(|e| format!("writing {}: {e}", plist.display()))?;
    // The captured env can include OMLX_API_KEY, so keep the plist owner-only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&plist, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("securing {}: {e}", plist.display()))?;
    }

    let target = format!("gui/{}", uid());
    let out = launchctl(&["bootstrap", &target, &plist.to_string_lossy()])?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(format!(
            "launchctl bootstrap failed: {}",
            if stderr.is_empty() {
                format!("exit {:?}", out.status.code())
            } else {
                stderr
            }
        ));
    }
    Ok((plist, env.into_iter().map(|(k, _)| k).collect()))
}

/// Stop and remove the LaunchAgent. Idempotent.
pub fn uninstall() -> Result<(), String> {
    bootout_if_loaded();
    let plist = plist_path().ok_or("HOME not set")?;
    match std::fs::remove_file(&plist) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("removing {}: {e}", plist.display())),
    }
}

/// Whether launchd currently knows about the job, and the plist path.
pub fn status() -> (bool, Option<PathBuf>, bool) {
    let plist = plist_path();
    let installed = plist.as_ref().is_some_and(|p| p.exists());
    let loaded = launchctl(&["print", &format!("gui/{}/{LABEL}", uid())])
        .map(|o| o.status.success())
        .unwrap_or(false);
    (installed, plist, loaded)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn plist_has_the_supervision_keys_that_make_it_24_7() {
        let p = render_plist(
            Path::new("/opt/agent24d"),
            Path::new("/logs/out.log"),
            Path::new("/logs/err.log"),
            &[],
        );
        assert!(p.contains("<string>ai.auraai.agent24</string>"));
        assert!(p.contains("<string>/opt/agent24d</string>"));
        // starts at login
        assert!(p.contains("<key>RunAtLoad</key>\n    <true/>"));
        // restarts a crash…
        assert!(p.contains("<key>KeepAlive</key>"));
        // …but NOT a clean stop: SuccessfulExit=false means "only if it failed"
        assert!(p.contains("<key>SuccessfulExit</key>\n        <false/>"));
        // crash loops back off
        assert!(p.contains("<key>ThrottleInterval</key>"));
        // output survives for diagnosis
        assert!(p.contains("/logs/out.log"));
        assert!(p.contains("/logs/err.log"));
    }

    #[test]
    fn daemon_is_launched_with_a_dynamic_port() {
        // The port must stay dynamic: the daemon publishes it via the state
        // file, and a hard-coded port would collide with a manually started one.
        let p = render_plist(Path::new("/a"), Path::new("/o"), Path::new("/e"), &[]);
        assert!(p.contains("<string>serve</string>"));
        assert!(p.contains("<string>--port</string>"));
        assert!(p.contains("<string>0</string>"));
    }

    #[test]
    fn paths_with_xml_metacharacters_do_not_corrupt_the_plist() {
        // A folder literally named "R&D <beta>" is legal on macOS and would
        // otherwise produce a plist launchd silently refuses to load.
        let p = render_plist(
            Path::new("/Users/x/R&D <beta>/agent24d"),
            Path::new("/o"),
            Path::new("/e"),
            &[],
        );
        assert!(p.contains("/Users/x/R&amp;D &lt;beta&gt;/agent24d"));
        assert!(!p.contains("R&D <beta>"));
    }

    #[test]
    fn captured_env_lands_in_the_plist_so_shell_exec_still_finds_tools() {
        // launchd's default PATH is only /usr/bin:/bin:/usr/sbin:/sbin, so
        // without this a shell_exec for `node` works by hand and fails at 3am.
        let env = vec![
            ("PATH".to_owned(), "/opt/homebrew/bin:/usr/bin".to_owned()),
            ("OMLX_URL".to_owned(), "http://127.0.0.1:8088".to_owned()),
        ];
        let p = render_plist(Path::new("/a"), Path::new("/o"), Path::new("/e"), &env);
        assert!(p.contains("<key>EnvironmentVariables</key>"));
        assert!(p.contains("<key>PATH</key>"));
        assert!(p.contains("<string>/opt/homebrew/bin:/usr/bin</string>"));
        assert!(p.contains("<key>OMLX_URL</key>"));
    }

    #[test]
    fn no_env_block_when_nothing_captured() {
        let p = render_plist(Path::new("/a"), Path::new("/o"), Path::new("/e"), &[]);
        assert!(!p.contains("EnvironmentVariables"));
    }

    #[test]
    fn env_values_are_xml_escaped_too() {
        let env = vec![(
            "A24_GUARDIAN_ALWAYS_REVIEW".to_owned(),
            "exec&<fs".to_owned(),
        )];
        let p = render_plist(Path::new("/a"), Path::new("/o"), Path::new("/e"), &env);
        assert!(p.contains("exec&amp;&lt;fs"));
        assert!(!p.contains("exec&<fs"));
    }

    /// Every environment variable the daemon side READS must be in
    /// `PASSTHROUGH_VARS`, because launchd hands a LaunchAgent none of the login
    /// shell's environment.
    ///
    /// This scans the SOURCE rather than listing the names a second time. The list
    /// it replaced was a hand-copy of the constant, so it could only fail if a
    /// name were deleted from the constant — it could never notice one that was
    /// never added, and that is exactly how this list drifted: the daemon read
    /// `A24_OS_PACKAGES` while the old test stayed green.
    ///
    /// It resolves `env::var(SOME_CONST)` as well as a string literal. That is not
    /// speculative generality: the first version only understood literals, and the
    /// very next commit turned one read into a `const` reference — the positive
    /// control below fired within the hour. A scanner that only sees one spelling
    /// of the thing it looks for goes quiet exactly when the code is refactored.
    #[test]
    fn passthrough_list_matches_what_the_daemon_actually_reads() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut sources: Vec<String> = Vec::new();
        for dir in ["apps/agent24d/src", "crates"] {
            walk_rs(&root.join(dir), &mut |path| {
                if let Ok(text) = std::fs::read_to_string(path) {
                    sources.push(text);
                }
            });
        }
        // Positive control #1: an empty corpus makes every assertion below vacuous,
        // and a renamed directory is how that happens with no other symptom.
        assert!(
            sources.len() > 10,
            "scanned only {} files — the paths are wrong",
            sources.len()
        );

        // `const NAME: &str = "VALUE";` — so an env read spelled as a constant can
        // be resolved back to the variable it names.
        let mut consts: Vec<(String, String)> = Vec::new();
        for text in &sources {
            let mut rest = text.as_str();
            while let Some(i) = rest.find("const ") {
                rest = &rest[i + "const ".len()..];
                let Some((decl, after)) = rest.split_once('=') else {
                    break;
                };
                if !decl.contains("&str") {
                    continue;
                }
                let Some(name) = decl.split(':').next().map(str::trim) else {
                    continue;
                };
                let after = after.trim_start();
                if let Some(lit) = after.strip_prefix('"').and_then(|r| r.split('"').next()) {
                    consts.push((name.to_owned(), lit.to_owned()));
                }
            }
        }

        let mut found: Vec<String> = Vec::new();
        for text in &sources {
            for pat in ["env::var(", "env::var_os("] {
                let mut rest = text.as_str();
                while let Some(i) = rest.find(pat) {
                    rest = &rest[i + pat.len()..];
                    let Some(end) = rest.find(')') else { break };
                    let arg = rest[..end].trim();
                    let name = match arg.strip_prefix('"').and_then(|r| r.split('"').next()) {
                        Some(lit) => Some(lit.to_owned()),
                        // A constant: resolve it, or fail loudly. Silently ignoring
                        // an unresolvable read is how this test would go quiet the
                        // next time the spelling changes again.
                        None => {
                            let ident = arg.rsplit("::").next().unwrap_or(arg);
                            let hit = consts
                                .iter()
                                .find(|(n, _)| n == ident)
                                .map(|(_, v)| v.clone());
                            assert!(
                                hit.is_some()
                                    || !ident.chars().all(|c| c.is_ascii_uppercase()
                                        || c.is_ascii_digit()
                                        || c == '_'),
                                "an env read spelled `{arg}` could not be resolved to a name; \
                                 this test cannot see what it forwards"
                            );
                            hit
                        }
                    };
                    // Two names are handled elsewhere and must not be demanded
                    // of `PASSTHROUGH_VARS`:
                    //
                    // - HOME: launchd sets it itself.
                    // - PATH: captured separately in `snapshot_env` because it
                    //   needs the LOGIN shell's value rather than whatever the
                    //   daemon happens to have. It IS forwarded — just not
                    //   through this list. A scanner that did not know that
                    //   reported it the moment ME-3b-3's `which()` began reading
                    //   PATH on the daemon side: the READING was right, the
                    //   conclusion would have been wrong.
                    if let Some(name) = name {
                        let shouty = !name.is_empty()
                            && name
                                .chars()
                                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
                        if shouty && !matches!(name.as_str(), "HOME" | "PATH") {
                            found.push(name);
                        }
                    }
                }
            }
        }

        // Positive control #2: a variable known to be read must come out of the
        // scan. This is what caught the literal-only version above.
        assert!(
            found.iter().any(|v| v == "A24_OS_PACKAGES"),
            "the scan did not find a variable known to be read: {found:?}"
        );
        found.sort();
        found.dedup();
        for v in &found {
            assert!(
                PASSTHROUGH_VARS.contains(&v.as_str()),
                "{v} is read by the daemon but is not in PASSTHROUGH_VARS; a \
                 LaunchAgent-started daemon would never see it"
            );
        }
    }

    fn walk_rs(dir: &Path, f: &mut impl FnMut(&Path)) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk_rs(&p, f);
            } else if p.extension().is_some_and(|x| x == "rs") {
                f(&p);
            }
        }
    }

    #[test]
    fn xml_escape_covers_all_five_entities() {
        assert_eq!(
            xml_escape(r#"&<>"'"#),
            "&amp;&lt;&gt;&quot;&apos;".to_owned()
        );
        assert_eq!(xml_escape("plain/path"), "plain/path");
    }

    #[test]
    fn plist_path_is_a_user_launch_agent() {
        // Must be a per-user LaunchAgent (no sudo, runs in the login session),
        // not a system-wide LaunchDaemon.
        let p = plist_path_in(Path::new("/Users/tester"));
        assert_eq!(
            p,
            PathBuf::from("/Users/tester/Library/LaunchAgents/ai.auraai.agent24.plist")
        );
    }

    #[test]
    fn install_refuses_a_missing_binary_with_actionable_advice() {
        let err = install(Path::new("/definitely/not/here/agent24d")).unwrap_err();
        assert!(err.contains("not found"), "{err}");
        assert!(
            err.contains("AGENT24D_BIN") || err.contains("cargo build"),
            "{err}"
        );
    }
}
