//! Fixtures for tests that build an OS package the daemon will scan.
//!
//! Production rules are unchanged. A real package's `spawn.command` must be a
//! native executable inside the package; an external interpreter (`python3`,
//! `node`, …) is rejected. [`native_launcher`] exists only so a test can keep
//! a script's handshake, exit code, and probe files: the launcher is a tiny
//! native binary inside the package, and at run time it `execv`s an interpreter
//! that lives outside the package. The kernel starts a module with the package
//! directory as its cwd, so a relative script path in `argv` is resolved
//! against that directory by the interpreter. Do not bake an absolute package
//! path into the launcher — `os install` copies the tree, and the copy must
//! still find its script.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::package_integrity;
use agent24_components::integrity::IntegrityError;

/// Compile `source` with the system `cc` and write the binary to `dir/rel`.
/// The `.c` file is removed before returning, so a later [`seal`] does not
/// hash it as payload.
pub fn compile_c(dir: &Path, rel: &str, source: &str) -> Result<(), String> {
    let dest = dir.join(rel);
    let parent = dest
        .parent()
        .ok_or_else(|| format!("no parent for {rel}"))?;
    fs::create_dir_all(parent).map_err(|err| format!("create {}: {err}", parent.display()))?;
    let source_path = dest.with_extension("c");
    fs::write(&source_path, source)
        .map_err(|err| format!("write {}: {err}", source_path.display()))?;
    let output = Command::new("cc")
        .arg("-O2")
        .arg("-o")
        .arg(&dest)
        .arg(&source_path)
        .output()
        .map_err(|err| format!("spawn cc: {err}"))?;
    let _ = fs::remove_file(&source_path);
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "cc failed to build {rel}: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Write a native launcher at `dir/rel` that `execv`s `argv`.
///
/// `argv[0]` is the interpreter. A bare name is resolved on `PATH` while the
/// launcher is compiled, and that absolute path is what `execv` receives —
/// `execv` does not search `PATH`. Later arguments are embedded as given.
/// Relative paths (a script inside the package) stay relative: the kernel's
/// cwd is the package directory, including after `os install` copies the tree.
///
/// The interpreter is not part of the package. This is a test double. Product
/// verification still rejects a package whose `spawn.command` is that
/// interpreter.
pub fn native_launcher(dir: &Path, rel: &str, argv: &[&str]) -> Result<(), String> {
    let program = argv
        .first()
        .ok_or_else(|| "native_launcher requires an interpreter".to_owned())?;
    let resolved = resolve_program(program)?;
    let mut quoted = Vec::with_capacity(argv.len());
    quoted.push(c_quote(&resolved.to_string_lossy())?);
    for arg in &argv[1..] {
        quoted.push(c_quote(arg)?);
    }
    let mut source = String::from(
        "/* test fixture: exec an interpreter that lives outside the package */\n\
         #include <unistd.h>\n\
         int main(void) {\n  char *argv[] = {",
    );
    for arg in &quoted {
        source.push_str(arg);
        source.push_str(", ");
    }
    source.push_str("0};\n  execv(argv[0], argv);\n  return 127;\n}\n");
    compile_c(dir, rel, &source)
}

/// Copy a one-byte-stable native program that exits 1 (the same status `false`
/// returns) to `dir/rel`. Every call writes the same bytes, so two packages
/// sealed from the same manifest get the same digest.
pub fn native_exit(dir: &Path, rel: &str) -> Result<(), String> {
    let bytes = exit_stub_bytes()?;
    let dest = dir.join(rel);
    let parent = dest
        .parent()
        .ok_or_else(|| format!("no parent for {rel}"))?;
    fs::create_dir_all(parent).map_err(|err| format!("create {}: {err}", parent.display()))?;
    fs::write(&dest, bytes).map_err(|err| format!("write {}: {err}", dest.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&dest)
            .map_err(|err| format!("stat {}: {err}", dest.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&dest, permissions)
            .map_err(|err| format!("chmod {}: {err}", dest.display()))?;
    }
    Ok(())
}

/// `package_integrity::generate` for a test package. `executables` are package-
/// relative paths and must include `spawn.command` when the manifest has one.
pub fn seal(dir: &Path, executables: &[&str]) -> Result<(), IntegrityError> {
    let owned: Vec<String> = executables.iter().map(|item| (*item).to_owned()).collect();
    package_integrity::generate(dir, &owned)
}

/// Launcher at `bin/launch` running `python3 -I -S mod.py`, then [`seal`].
/// The manifest's `spawn.command` must already be `bin/launch`, and `mod.py`
/// must already be in the package.
pub fn seal_python_script(dir: &Path) -> Result<(), String> {
    native_launcher(dir, "bin/launch", &["python3", "-I", "-S", "mod.py"])?;
    seal(dir, &["bin/launch"]).map_err(|err| err.to_string())
}

/// Like [`seal_python_script`], with the caller's full `execv` argument vector
/// (interpreter first). The launcher is still `bin/launch`.
pub fn seal_python_argv(dir: &Path, argv: &[&str]) -> Result<(), String> {
    native_launcher(dir, "bin/launch", argv)?;
    seal(dir, &["bin/launch"]).map_err(|err| err.to_string())
}

/// Native exit-1 stub at `rel`, then [`seal`] listing that path.
pub fn seal_exit_stub(dir: &Path, rel: &str) -> Result<(), String> {
    native_exit(dir, rel)?;
    seal(dir, &[rel]).map_err(|err| err.to_string())
}

/// `generate` publishes `domain-os.yml` as mode 0400. Tests that append to it
/// and seal again need it writable first.
pub fn make_writable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .map_err(|err| format!("stat {}: {err}", path.display()))?
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions)
            .map_err(|err| format!("chmod {}: {err}", path.display()))?;
    }
    Ok(())
}

fn exit_stub_bytes() -> Result<Vec<u8>, String> {
    static ONCE: OnceLock<Result<Vec<u8>, String>> = OnceLock::new();
    ONCE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!(
            "a24-exit-stub-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).map_err(|err| format!("create {}: {err}", dir.display()))?;
        let compiled = compile_c(&dir, "stub", "int main(void) { return 1; }\n");
        let bytes = compiled
            .and_then(|_| fs::read(dir.join("stub")).map_err(|err| format!("read stub: {err}")));
        let _ = fs::remove_dir_all(&dir);
        bytes
    })
    .clone()
}

/// The launcher runs with the package directory as its cwd, so the path baked
/// into it must be absolute; and like a shell's PATH search, a non-executable
/// file earlier on PATH does not hide a usable one later.
fn resolve_program(name: &str) -> Result<PathBuf, String> {
    let path = Path::new(name);
    let bare = path.components().count() == 1 && !name.contains('/') && !name.contains('\\');
    let absolute = |candidate: &Path| {
        std::path::absolute(candidate)
            .map_err(|err| format!("absolute path of {}: {err}", candidate.display()))
    };
    if !bare {
        if is_executable(path) {
            return absolute(path);
        }
        return Err(format!("interpreter not found: {name}"));
    }
    let paths = std::env::var_os("PATH").ok_or_else(|| "PATH is unset".to_owned())?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return absolute(&candidate);
        }
    }
    Err(format!("interpreter {name} is not on PATH"))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn c_quote(value: &str) -> Result<String, String> {
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' | '"' => {
                out.push('\\');
                out.push(character);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if other.is_control() => {
                return Err(format!("cannot embed {other:?} in a test launcher"));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    Ok(out)
}
