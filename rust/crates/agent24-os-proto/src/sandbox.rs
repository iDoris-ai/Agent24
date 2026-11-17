//! Seatbelt profile for one OS module (SBX-1).
//!
//! A pure function of the paths the module is about to be given. The trampoline
//! hands the text to `/usr/bin/sandbox-exec -p`. macOS only applies it; the
//! function itself has no process and no `unsafe`.

use std::path::{Path, PathBuf};

/// What [`seatbelt_profile`] needs. Every path is canonicalised (`/var` and
/// `/tmp` become `/private/var` and `/private/tmp` on macOS) before it is
/// written into the profile.
#[derive(Debug)]
pub struct SeatbeltInputs<'a> {
    pub package_dir: &'a Path,
    pub data_dir: &'a Path,
    pub callback_sock: &'a Path,
    pub program: &'a Path,
    /// Other binaries this process execs by an absolute path baked into
    /// `program` (a test launcher's interpreter). Same prefix rule as
    /// `program`.
    pub also_programs: &'a [&'a Path],
    pub temp_dir: &'a Path,
    /// The user's home. Its contents stay unreadable, except a program prefix
    /// that already lives there (a `pyenv` or `fnm` install) and the data
    /// directory when that is under the home.
    pub home_dir: Option<&'a Path>,
}

/// The profile could not be built. Starting the module anyway would run it
/// with no isolation, so the caller refuses.
#[derive(Debug)]
pub struct ProfileError(pub String);

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Read roots that are not the module's own trees. `/opt/homebrew` and
/// `/usr/local` are where a PATH interpreter keeps its libraries.
const SYSTEM_READ: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/System",
    "/Library",
    "/private/etc",
    "/dev",
    "/opt/homebrew",
    "/usr/local",
];

/// Devices a runtime writes without them being the module's data.
const WRITE_DEVICES: &[&str] = &["/dev/null", "/dev/zero", "/dev/dtracehelper"];

/// The seatbelt profile for one module.
///
/// # Errors
///
/// [`ProfileError`] when a path cannot be canonicalised or contains a
/// character the profile syntax cannot quote.
pub fn seatbelt_profile(inputs: &SeatbeltInputs<'_>) -> Result<String, ProfileError> {
    let package = canonical(inputs.package_dir)?;
    let data = canonical(inputs.data_dir)?;
    let temp = canonical(inputs.temp_dir)?;
    let callback = canonical(inputs.callback_sock)?;
    let home = inputs
        .home_dir
        .map(canonical)
        .transpose()?
        .filter(|p| p != Path::new("/"));

    let mut reads = Vec::new();
    for root in SYSTEM_READ {
        push_unique(&mut reads, PathBuf::from(root));
    }
    // `/usr/bin/python3` on macOS is a stub that reads this symlink and then
    // the toolchain it names. Absent on machines without developer tools.
    for tools in developer_tools() {
        push_unique(&mut reads, tools);
    }
    push_unique(&mut reads, package);
    push_unique(&mut reads, data.clone());
    push_unique(&mut reads, temp.clone());
    for path in std::iter::once(inputs.program).chain(inputs.also_programs.iter().copied()) {
        add_program(&mut reads, path, home.as_deref())?;
    }

    let data_dir = data.clone();
    let mut writes = vec![data, temp];
    for dev in WRITE_DEVICES {
        push_unique(&mut writes, PathBuf::from(*dev));
    }

    let mut callbacks = vec![callback];
    if let Some(raw) = absolute(inputs.callback_sock) {
        push_unique(&mut callbacks, raw);
    }

    let mut profile = String::from(
        "(version 1)\n\
         (deny default)\n\
         (allow process-exec)\n\
         (allow process-fork)\n\
         (allow signal (target same-sandbox))\n\
         (allow sysctl-read)\n\
         (allow mach-lookup)\n\
         (allow file-ioctl)\n\
         (allow file-read-metadata\n    (subpath \"/\")\n)\n\
         (allow file-read*\n    (literal \"/\")\n",
    );
    for path in &reads {
        profile.push_str("    (subpath ");
        profile.push_str(&quote(path)?);
        profile.push_str(")\n");
    }
    profile.push_str(")\n(allow file-map-executable\n");
    for path in &reads {
        profile.push_str("    (subpath ");
        profile.push_str(&quote(path)?);
        profile.push_str(")\n");
    }
    profile.push_str(")\n(allow file-write*\n");
    for path in &writes {
        let filter = if WRITE_DEVICES.contains(&path.to_str().unwrap_or("")) {
            "literal"
        } else {
            "subpath"
        };
        profile.push_str("    (");
        profile.push_str(filter);
        profile.push(' ');
        profile.push_str(&quote(path)?);
        profile.push_str(")\n");
    }
    // Writes inside the data directory stay allowed; the directory node itself
    // cannot be renamed, removed or replaced (#1863 B2). Later rules win.
    profile.push_str(")\n(deny file-write*\n    (literal ");
    profile.push_str(&quote(&data_dir)?);
    profile.push_str(")\n");
    // D1 (documents' Office → PDF): LibreOffice's single-instance pipe is a
    // socket it can only create at `/tmp/OSL_PIPE_*`, or `/var/tmp/OSL_PIPE_*`
    // when `/tmp` is not writable (osl `pipe.cxx`, hard-coded); without it
    // `soffice --convert-to` exits 1. Only that name, and the directory node
    // (its writability is checked first), never anything else in it.
    profile.push_str(
        ")\n(allow file-write*\n    (literal \"/private/var/tmp\")\n    (regex #\"^/private/var/tmp/OSL_PIPE_\")\n",
    );
    profile.push_str(
        ")\n\
         (allow network-inbound\n    (local unix-socket)\n)\n\
         (allow network-bind\n    (local unix-socket)\n)\n\
         (allow network-outbound\n",
    );
    for path in &callbacks {
        profile.push_str("    (remote unix-socket (path-literal ");
        profile.push_str(&quote(path)?);
        profile.push_str("))\n");
    }
    profile.push_str(")\n");
    Ok(profile)
}

/// Refuse a data directory the module could have redirected (#1863 B2). The
/// module can write its data directory, including replacing the directory
/// itself with a symlink between two runs; the next profile would then follow
/// that link and grant writes wherever it points. Checked on every start,
/// right before the profile is built: the leaf must be a real directory, and
/// its canonical path must not be `/`, the home, or an ancestor of the home.
/// Returns that canonical path, for the profile to use as is. The profile also
/// denies writes to the directory node itself, so a module (or a process left
/// from an earlier run) cannot rename or replace it at all.
pub fn check_data_dir(data_dir: &Path, home: Option<&Path>) -> Result<PathBuf, ProfileError> {
    let meta = std::fs::symlink_metadata(data_dir).map_err(|e| {
        ProfileError(format!(
            "cannot inspect the data directory {}: {e}",
            data_dir.display()
        ))
    })?;
    if !meta.file_type().is_dir() {
        return Err(ProfileError(format!(
            "the data directory {} is not a plain directory (a symlink?)",
            data_dir.display()
        )));
    }
    let data = canonical(data_dir)?;
    let home = home.map(canonical).transpose()?;
    if data == Path::new("/") || home.is_some_and(|home| home.starts_with(&data)) {
        return Err(ProfileError(format!(
            "the data directory {} resolves to {}, which would expose the home",
            data_dir.display(),
            data.display()
        )));
    }
    Ok(data)
}

/// Canonicalise `path`, keeping a trailing component that does not exist yet
/// (the data directory, or the socket, created around spawn).
pub fn canonical(path: &Path) -> Result<PathBuf, ProfileError> {
    let mut rest = Vec::new();
    let mut cur = path.to_path_buf();
    loop {
        if let Ok(found) = cur.canonicalize() {
            let mut out = found;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return Ok(out);
        }
        let Some(name) = cur.file_name().map(std::ffi::OsStr::to_os_string) else {
            return Err(ProfileError(format!(
                "could not canonicalise {}",
                path.display()
            )));
        };
        rest.push(name);
        if !cur.pop() {
            return Err(ProfileError(format!(
                "could not canonicalise {}",
                path.display()
            )));
        }
    }
}

/// Read prefixes for one program: its canonical path, and the path it was
/// given when that is a different absolute spelling (a symlink). Seatbelt
/// checks the path handed to `exec`, not only the vnode it resolves to.
fn add_program(
    reads: &mut Vec<PathBuf>,
    path: &Path,
    home: Option<&Path>,
) -> Result<(), ProfileError> {
    let program = canonical(path)?;
    for dir in program_prefixes(&program, home) {
        push_unique(reads, dir);
    }
    if let Some(raw) = absolute(path)
        && raw != program
    {
        for dir in program_prefixes(&raw, home) {
            push_unique(reads, dir);
        }
    }
    Ok(())
}

/// Parent and grandparent of `program`, so a binary can read the libraries
/// next to it (`bin/` + `lib/` under one prefix). Never `/` and never the
/// home directory itself — that would make the rest of the home readable.
fn program_prefixes(program: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Some(parent) = program.parent() else {
        return out;
    };
    push_prefix(&mut out, parent, home);
    if let Some(grand) = parent.parent() {
        push_prefix(&mut out, grand, home);
    }
    out
}

fn push_prefix(out: &mut Vec<PathBuf>, dir: &Path, home: Option<&Path>) {
    if dir.as_os_str().is_empty() || dir == Path::new("/") {
        return;
    }
    if home.is_some_and(|home| dir == home) {
        return;
    }
    push_unique(out, dir.to_path_buf());
}

fn push_unique(out: &mut Vec<PathBuf>, path: PathBuf) {
    if path.as_os_str().is_empty() || out.iter().any(|have| have == &path) {
        return;
    }
    out.push(path);
}

fn absolute(path: &Path) -> Option<PathBuf> {
    path.is_absolute().then(|| path.to_path_buf())
}

/// The macOS `/usr/bin/python3` stub reads `/var/select/developer_dir` and then
/// the toolchain that link names. Nothing is added when the link is absent.
fn developer_tools() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(dir) = canonical(Path::new("/var/select")) {
        out.push(dir);
    }
    if let Ok(target) = canonical(Path::new("/var/select/developer_dir"))
        && !target.starts_with("/usr")
        && !target.starts_with("/Library")
        && !target.starts_with("/System")
    {
        out.push(target);
    }
    out
}

fn quote(path: &Path) -> Result<String, ProfileError> {
    let text = path.to_str().ok_or_else(|| {
        ProfileError(format!(
            "path {} is not valid UTF-8, so it cannot go in a seatbelt profile",
            path.display()
        ))
    })?;
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '\\' | '"' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => {
                return Err(ProfileError(format!(
                    "path {text} contains a control character, so it cannot go in a seatbelt profile"
                )));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn inputs<'a>(
        package: &'a Path,
        data: &'a Path,
        callback: &'a Path,
        program: &'a Path,
        temp: &'a Path,
        home: Option<&'a Path>,
    ) -> SeatbeltInputs<'a> {
        SeatbeltInputs {
            package_dir: package,
            data_dir: data,
            callback_sock: callback,
            program,
            also_programs: &[],
            temp_dir: temp,
            home_dir: home,
        }
    }

    /// #1863 B2: under the generated profile the module writes inside its
    /// data directory but cannot move the directory itself out of the way.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_data_dir_itself_cannot_be_moved_under_the_profile() {
        let t = tempfile::tempdir().unwrap();
        let root = canonical(t.path()).unwrap();
        let package = root.join("pkg");
        let data = root.join("data");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let sh = Path::new("/bin/sh");
        let profile = seatbelt_profile(&inputs(
            &package,
            &data,
            &root.join("cb.sock"),
            sh,
            &std::env::temp_dir(),
            None,
        ))
        .unwrap();
        let run = |script: &str| {
            std::process::Command::new("/usr/bin/sandbox-exec")
                .arg("-p")
                .arg(&profile)
                .arg(sh)
                .arg("-c")
                .arg(script)
                .current_dir(&data)
                .status()
                .unwrap()
                .success()
        };
        let d = data.display();
        assert!(run(&format!("echo x > '{d}/f' && rm '{d}/f'")));
        assert!(!run(&format!("/bin/mv '{d}' '{d}.old'")));
        assert!(data.is_dir() && !root.join("data.old").exists());
    }

    #[test]
    fn a_data_dir_swapped_for_a_symlink_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let data = t.path().join("data");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        check_data_dir(&data, Some(&home)).unwrap();

        std::fs::remove_dir(&data).unwrap();
        std::os::unix::fs::symlink(&home, &data).unwrap();
        let err = check_data_dir(&data, Some(&home)).unwrap_err();
        assert!(err.0.contains("not a plain directory"), "{err}");

        let err = check_data_dir(&home, Some(&home)).unwrap_err();
        assert!(err.0.contains("expose the home"), "{err}");
        let err = check_data_dir(t.path(), Some(&home)).unwrap_err();
        assert!(err.0.contains("expose the home"), "{err}");
    }

    #[test]
    fn a_quote_and_a_backslash_in_a_path_are_escaped() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("pkg");
        let data = root.path().join("a\"b\\c");
        let temp = root.path().join("tmp");
        std::fs::create_dir(&package).unwrap();
        std::fs::create_dir(&data).unwrap();
        std::fs::create_dir(&temp).unwrap();
        let callback = temp.join("cb.sock");
        let profile = seatbelt_profile(&inputs(
            &package,
            &data,
            &callback,
            Path::new("/bin/sh"),
            &temp,
            None,
        ))
        .unwrap();
        assert!(
            profile.contains(r#"(subpath "/"#) && profile.contains(r#"a\"b\\c")"#),
            "{profile}"
        );
        assert!(!profile.contains("a\"b\\c\")"), "{profile}");
    }

    #[test]
    fn var_and_tmp_are_canonicalised_and_the_home_is_not_readable() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("pkg");
        let data = root.path().join("data");
        std::fs::create_dir(&package).unwrap();
        // Data is named but not created: canonicalise keeps the missing leaf.
        let temp = std::env::temp_dir();
        let callback = temp.join(format!("a24-sbx-{}.sock", std::process::id()));
        let home = PathBuf::from("/Users/sbx-home-not-real");
        let profile = seatbelt_profile(&inputs(
            &package,
            &data,
            &callback,
            Path::new("/bin/sh"),
            &temp,
            Some(&home),
        ))
        .unwrap();
        let canon_temp = canonical(&temp).unwrap();
        assert!(
            profile.contains(&canon_temp.display().to_string()),
            "temp {canon_temp:?} not in profile"
        );
        // On macOS the temp directory is `/var/folders/…` and canonicalises
        // under `/private`. The profile must carry the canonical spelling.
        if canon_temp.starts_with("/private/") {
            assert!(
                !profile.contains(&format!("(subpath \"{}\")", temp.display())),
                "non-canonical temp path leaked into a subpath filter"
            );
        }
        let canon_data = canonical(&data).unwrap();
        assert!(profile.contains(&format!("(subpath \"{}\")", canon_data.display())));
        assert!(
            !profile.contains("(subpath \"/Users/sbx-home-not-real\")"),
            "{profile}"
        );
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(literal \"/\")"));
        assert!(profile.contains("(allow network-outbound"));
        assert!(!profile.contains("network-outbound\n    (remote ip"));
        assert!(profile.contains("(literal \"/dev/null\")"));
        // The package is readable and not a write root of its own when the
        // data directory is a sibling.
        let canon_pkg = canonical(&package).unwrap();
        assert!(profile.contains(&format!("(subpath \"{}\")", canon_pkg.display())));
        let write = profile.split("(allow file-write*").nth(1).unwrap();
        let write = write.split(")\n(allow network").next().unwrap();
        assert!(
            !write.contains(&canon_pkg.display().to_string()),
            "package is writable: {write}"
        );
    }

    #[test]
    fn a_program_prefix_under_the_home_is_readable_and_the_home_itself_is_not() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("pkg");
        let data = root.path().join("data");
        let temp = root.path().join("tmp");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&temp).unwrap();
        let home = root.path().join("home");
        let program = home.join("rt/bin/node");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, b"").unwrap();
        let profile = seatbelt_profile(&inputs(
            &package,
            &data,
            &temp.join("cb.sock"),
            &program,
            &temp,
            Some(&home),
        ))
        .unwrap();
        let prefix = canonical(program.parent().unwrap().parent().unwrap()).unwrap();
        assert!(
            profile.contains(&format!("(subpath \"{}\")", prefix.display())),
            "{profile}"
        );
        let home = canonical(&home).unwrap();
        assert!(
            !profile.contains(&format!("(subpath \"{}\")", home.display())),
            "the home itself became readable: {profile}"
        );
    }
}
