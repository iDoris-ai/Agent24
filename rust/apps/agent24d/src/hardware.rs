//! Startup hardware snapshot (HW-1, ADR-036): probe once at daemon start and
//! write `hardware.json` (profile + level + usable_bytes + reasons) to the
//! state directory. Best-effort — a failure is logged and never blocks
//! startup. No HTTP route here (that is HW-3).

use std::path::Path;

use agent24_hw::{HardwareProbe, HardwareReport, SystemProbe};

pub const HARDWARE_FILE: &str = "hardware.json";

/// Atomic write: temp file in the same directory, then rename.
pub fn write_report(dir: &Path, report: &HardwareReport) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(report).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("{HARDWARE_FILE}.tmp.{}", std::process::id()));
    std::fs::write(&tmp, &json).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, dir.join(HARDWARE_FILE)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename {}: {e}", tmp.display())
    })
}

/// Probe now (blocking: up to a few short subprocesses) and persist.
pub fn snapshot_blocking(dir: &Path, probe: &dyn HardwareProbe) -> Result<HardwareReport, String> {
    let report = HardwareReport::from_env(probe);
    write_report(dir, &report)?;
    Ok(report)
}

/// Fire-and-forget from the async startup path.
pub fn spawn_startup_snapshot(dir: std::path::PathBuf) {
    tokio::task::spawn_blocking(move || match snapshot_blocking(&dir, &SystemProbe::new()) {
        Ok(r) => tracing::info!(
            level = ?r.level,
            usable_bytes = r.usable_bytes,
            "hardware snapshot written to {}",
            dir.join(HARDWARE_FILE).display()
        ),
        Err(err) => tracing::warn!("hardware snapshot failed (ignored): {err}"),
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_hw::{Accelerator, FakeHardwareProbe, HardwareProfile};

    fn profile() -> HardwareProfile {
        HardwareProfile {
            total_mem_bytes: 16 * 1024 * 1024 * 1024,
            avail_mem_bytes: 8 * 1024 * 1024 * 1024,
            arch: "aarch64".to_owned(),
            cpu_cores: 8,
            accelerator: Accelerator::Metal,
            os: "macos".to_owned(),
            os_version: None,
            chip: Some("Apple M2".to_owned()),
            unified_memory: true,
            gpu_wired_limit_bytes: None,
            discrete_gpus: vec![],
            disks: vec![],
            models_dir_free_bytes: Some(100),
            on_battery: None,
            runtimes: vec![],
        }
    }

    #[test]
    fn writes_report_atomically_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let report = snapshot_blocking(dir.path(), &FakeHardwareProbe(profile())).unwrap();
        let text = std::fs::read_to_string(dir.path().join(HARDWARE_FILE)).unwrap();
        let back: HardwareReport = serde_json::from_str(&text).unwrap();
        assert_eq!(back, report);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![HARDWARE_FILE.to_owned()]);
    }

    #[test]
    fn unwritable_dir_is_an_error_not_a_panic() {
        let err = write_report(
            Path::new("/nonexistent-a24-dir/x"),
            &HardwareReport::new(profile(), None),
        );
        assert!(err.is_err());
    }
}
