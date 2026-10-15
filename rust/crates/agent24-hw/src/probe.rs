//! [`HardwareProbe`] seam, the real [`SystemProbe`], and the test double.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sysinfo::{Disks, System};

use crate::disks::{free_bytes_for_path, is_user_visible_volume};
use crate::parse;
use crate::{Accelerator, DiskInfo, GpuInfo, HardwareProfile};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);

/// The seam between pure policy and the real machine.
pub trait HardwareProbe {
    fn probe(&self) -> HardwareProfile;
}

/// Test double returning a fixed profile. Not behind `cfg(test)`: other
/// crates' tests use it.
pub struct FakeHardwareProbe(pub HardwareProfile);

impl HardwareProbe for FakeHardwareProbe {
    fn probe(&self) -> HardwareProfile {
        self.0.clone()
    }
}

/// `~/.omlx/models` (None when `HOME` is unset).
pub fn default_models_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".omlx").join("models"))
}

/// Real probe: sysinfo + a few platform commands, each with a 2 s timeout.
pub struct SystemProbe {
    models_dir: Option<PathBuf>,
}

impl SystemProbe {
    /// Models directory defaults to `~/.omlx/models`.
    pub fn new() -> Self {
        Self {
            models_dir: default_models_dir(),
        }
    }

    /// Probe with an explicit models directory.
    pub fn with_models_dir(models_dir: impl Into<PathBuf>) -> Self {
        Self {
            models_dir: Some(models_dir.into()),
        }
    }
}

impl Default for SystemProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl HardwareProbe for SystemProbe {
    fn probe(&self) -> HardwareProfile {
        let sys = System::new_all();
        let disks = Disks::new_with_refreshed_list();

        let all: Vec<DiskInfo> = disks
            .list()
            .iter()
            .map(|d| DiskInfo {
                name: d.name().to_string_lossy().into_owned(),
                mount_point: d.mount_point().to_string_lossy().into_owned(),
                total_bytes: d.total_space(),
                free_bytes: d.available_space(),
                removable: d.is_removable(),
                read_only: d.is_read_only(),
            })
            .collect();
        let fs_types: Vec<String> = disks
            .list()
            .iter()
            .map(|d| d.file_system().to_string_lossy().into_owned())
            .collect();
        let visible: Vec<DiskInfo> = all
            .iter()
            .zip(fs_types.iter())
            .filter(|(d, fs)| {
                is_user_visible_volume(&d.mount_point, &d.name, fs, d.total_bytes, d.read_only)
            })
            .map(|(d, _)| d.clone())
            .collect();

        let models_dir_free_bytes = self
            .models_dir
            .as_deref()
            .map(resolve_existing_prefix)
            .and_then(|p| free_bytes_for_path(&visible, &all, &p));

        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        let is_macos = os == "macos";
        let is_linux = os == "linux";

        let discrete_gpus: Vec<GpuInfo> = if is_linux {
            run_with_timeout(
                "nvidia-smi",
                &[
                    "--query-gpu=name,memory.total",
                    "--format=csv,noheader,nounits",
                ],
            )
            .map(|o| parse::parse_nvidia_smi(&o))
            .unwrap_or_default()
        } else {
            Vec::new()
        };

        HardwareProfile {
            total_mem_bytes: sys.total_memory(),
            avail_mem_bytes: sys.available_memory(),
            arch: arch.to_owned(),
            cpu_cores: u32::try_from(sys.cpus().len()).unwrap_or(0),
            accelerator: detect_accelerator(),
            os: os.to_owned(),
            os_version: if is_macos {
                run_with_timeout("sw_vers", &["-productVersion"])
                    .and_then(|o| parse::parse_product_version(&o))
            } else {
                System::os_version()
            },
            chip: if is_macos {
                run_with_timeout("sysctl", &["-n", "machdep.cpu.brand_string"])
                    .and_then(|o| parse::parse_brand_string(&o))
            } else {
                sys.cpus()
                    .first()
                    .map(|c| c.brand().trim().to_owned())
                    .filter(|b| !b.is_empty())
            },
            unified_memory: is_macos && arch == "aarch64",
            gpu_wired_limit_bytes: if is_macos {
                run_with_timeout("sysctl", &["-n", "iogpu.wired_limit_mb"])
                    .and_then(|o| parse::parse_wired_limit_mb(&o))
            } else {
                None
            },
            discrete_gpus,
            disks: visible,
            models_dir_free_bytes,
            on_battery: detect_on_battery(is_macos, is_linux),
            runtimes: Vec::new(),
        }
    }
}

fn detect_accelerator() -> Accelerator {
    if std::env::consts::OS == "macos" && std::env::consts::ARCH == "aarch64" {
        return Accelerator::Metal;
    }
    if std::env::consts::OS == "linux" && Path::new("/proc/driver/nvidia").exists() {
        return Accelerator::Cuda;
    }
    Accelerator::None
}

fn detect_on_battery(is_macos: bool, is_linux: bool) -> Option<bool> {
    if is_macos {
        return run_with_timeout("pmset", &["-g", "batt"])
            .and_then(|o| parse::parse_pmset_batt(&o));
    }
    if is_linux {
        return read_linux_power_supplies().and_then(|s| parse::on_battery_from_power_supplies(&s));
    }
    None
}

fn read_linux_power_supplies() -> Option<Vec<parse::PowerSupply>> {
    let rd = std::fs::read_dir("/sys/class/power_supply").ok()?;
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let dir = entry.path();
        let read = |name: &str| {
            std::fs::read_to_string(dir.join(name))
                .ok()
                .map(|s| s.trim().to_owned())
        };
        let Some(kind) = read("type") else { continue };
        out.push(parse::PowerSupply {
            kind,
            status: read("status"),
            online: read("online").map(|v| v == "1"),
        });
    }
    Some(out)
}

/// Canonicalize the longest existing ancestor of `path` and re-append the
/// missing tail, so a not-yet-created models dir (or a symlinked parent)
/// still maps to the right volume.
fn resolve_existing_prefix(path: &Path) -> PathBuf {
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(canon) = cur.canonicalize() {
            let mut out = canon;
            for part in tail.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name);
                cur = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Runs `program args…`, returns stdout on exit status 0 within 2 s; any
/// failure (missing binary, non-zero exit, timeout) is `None`.
fn run_with_timeout(program: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(program)
        // Hardware commands need executable lookup and stable parseable output,
        // never gateway/backend credentials inherited from the daemon.
        .env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin:/usr/sbin:/sbin".into()),
        )
        .env("LC_ALL", "C")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut buf = String::new();
                child.stdout.take()?.read_to_string(&mut buf).ok()?;
                return Some(buf);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_probe_returns_plausible_values() {
        let p = SystemProbe::new().probe();
        assert!(p.cpu_cores >= 1);
        assert!(p.total_mem_bytes > 0);
        assert!(!p.arch.is_empty());
        assert!(!p.os.is_empty());
        assert!(p.runtimes.is_empty());
    }

    #[test]
    fn fake_probe_roundtrip() {
        let mut p = SystemProbe::new().probe();
        p.on_battery = Some(true);
        assert_eq!(FakeHardwareProbe(p.clone()).probe(), p);
    }

    #[test]
    fn hardware_commands_do_not_inherit_daemon_secrets() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = "nonsecret-hardware-probe-fixture";
        if std::env::vars_os().any(|(k, v)| k == "IDORIS_API_KEY" && v == fixture) {
            let output = run_with_timeout(
                "sh",
                &[
                    "-c",
                    "test -z \"${IDORIS_API_KEY:-}\" && test -z \"${UNRELATED_FIXTURE_SECRET:-}\" && test -n \"$PATH\" && test \"$LC_ALL\" = C && printf isolated",
                ],
            );
            assert_eq!(output.as_deref(), Some("isolated"));
            return Ok(());
        }
        for missing_path in [false, true] {
            let mut command = Command::new(std::env::current_exe()?);
            command
                .args([
                    "--exact",
                    "probe::tests::hardware_commands_do_not_inherit_daemon_secrets",
                    "--nocapture",
                ])
                .env("IDORIS_API_KEY", fixture)
                .env("UNRELATED_FIXTURE_SECRET", fixture);
            if missing_path {
                command.env_remove("PATH");
            }
            let child = command.output()?;
            assert!(
                child.status.success(),
                "{}",
                String::from_utf8_lossy(&child.stderr)
            );
        }
        Ok(())
    }

    #[test]
    fn missing_command_is_none() {
        assert_eq!(
            run_with_timeout("definitely-not-a-real-binary-a24", &[]),
            None
        );
    }

    #[test]
    fn slow_command_times_out() {
        let t = Instant::now();
        assert_eq!(run_with_timeout("sleep", &["5"]), None);
        assert!(t.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn nonzero_exit_is_none() {
        assert_eq!(run_with_timeout("false", &[]), None);
    }

    #[test]
    fn nonexistent_models_dir_resolves_to_existing_ancestor() {
        let p = resolve_existing_prefix(Path::new("/tmp/a24-hw-nonexistent/x/y"));
        assert!(p.ends_with("a24-hw-nonexistent/x/y"));
    }

    #[test]
    fn models_dir_on_this_machine_has_free_bytes() {
        let p = SystemProbe::with_models_dir("/tmp/a24-hw-nonexistent/models").probe();
        assert!(p.models_dir_free_bytes.is_some());
    }
}
