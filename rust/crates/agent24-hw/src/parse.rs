//! Pure parsers for external-command output. No I/O — every function takes
//! the raw text and returns `None`/skips on bad input, never panics.

use crate::GpuInfo;

const MIB: u64 = 1024 * 1024;

/// `sysctl -n machdep.cpu.brand_string` → `Apple M4`.
pub fn parse_brand_string(out: &str) -> Option<String> {
    first_nonempty_line(out)
}

/// `sw_vers -productVersion` → `15.4`.
pub fn parse_product_version(out: &str) -> Option<String> {
    first_nonempty_line(out)
}

fn first_nonempty_line(out: &str) -> Option<String> {
    out.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_owned)
}

/// `sysctl -n iogpu.wired_limit_mb` (also accepts `iogpu.wired_limit_mb: N`
/// and `iogpu.wired_limit_mb = N`). `0` means "system default" → `None`.
/// Returns bytes.
pub fn parse_wired_limit_mb(out: &str) -> Option<u64> {
    let line = first_nonempty_line(out)?;
    let value = line.rsplit([':', '=']).next().unwrap_or(&line).trim();
    let mb: u64 = value.parse().ok()?;
    if mb == 0 { None } else { mb.checked_mul(MIB) }
}

/// `pmset -g batt`: first line is `Now drawing from 'Battery Power'` or
/// `Now drawing from 'AC Power'`. Anything else → `None` (unknown).
pub fn parse_pmset_batt(out: &str) -> Option<bool> {
    let first = out.lines().next()?;
    if first.contains("'Battery Power'") {
        Some(true)
    } else if first.contains("'AC Power'") || first.contains("'UPS Power'") {
        Some(false)
    } else {
        None
    }
}

/// `nvidia-smi --query-gpu=name,memory.total --format=csv,noheader,nounits`
/// → one `name, MiB` line per GPU. Malformed lines are skipped; a line with
/// a name but an unparsable memory column keeps the name with `vram_bytes: None`.
pub fn parse_nvidia_smi(out: &str) -> Vec<GpuInfo> {
    out.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            match line.rsplit_once(',') {
                Some((name, mem)) => {
                    let name = name.trim();
                    if name.is_empty() {
                        return None;
                    }
                    let vram_bytes = mem
                        .trim()
                        .parse::<u64>()
                        .ok()
                        .and_then(|mib| mib.checked_mul(MIB));
                    Some(GpuInfo {
                        name: name.to_owned(),
                        vram_bytes,
                    })
                }
                None => {
                    // Not CSV at all (e.g. "No devices were found").
                    None
                }
            }
        })
        .collect()
}

/// One entry of `/sys/class/power_supply/*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerSupply {
    /// Contents of the `type` file (`Mains`, `Battery`, `USB`, ...).
    pub kind: String,
    /// `status` file (`Discharging`, `Charging`, `Full`, ...), if any.
    pub status: Option<String>,
    /// `online` file as a bool, if any.
    pub online: Option<bool>,
}

/// Linux: `Some(false)` when a mains supply is online; `Some(true)` when a
/// battery is discharging and no mains supply is online; `None` otherwise
/// (no battery at all = desktop, or unreadable).
pub fn on_battery_from_power_supplies(supplies: &[PowerSupply]) -> Option<bool> {
    let mains_online = supplies
        .iter()
        .any(|s| s.kind.eq_ignore_ascii_case("Mains") && s.online == Some(true));
    if mains_online {
        return Some(false);
    }
    let discharging = supplies.iter().any(|s| {
        s.kind.eq_ignore_ascii_case("Battery")
            && s.status
                .as_deref()
                .is_some_and(|st| st.eq_ignore_ascii_case("Discharging"))
    });
    if discharging {
        return Some(true);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_string() {
        assert_eq!(
            parse_brand_string("Apple M4\n").as_deref(),
            Some("Apple M4")
        );
        assert_eq!(parse_brand_string("  \n\n"), None);
        assert_eq!(parse_brand_string(""), None);
    }

    #[test]
    fn wired_limit() {
        assert_eq!(parse_wired_limit_mb("0\n"), None);
        assert_eq!(parse_wired_limit_mb("20480\n"), Some(20480 * MIB));
        assert_eq!(
            parse_wired_limit_mb("iogpu.wired_limit_mb: 8192"),
            Some(8192 * MIB)
        );
        assert_eq!(parse_wired_limit_mb("iogpu.wired_limit_mb = 0"), None);
        assert_eq!(parse_wired_limit_mb("sysctl: unknown oid"), None);
        assert_eq!(parse_wired_limit_mb("-5"), None);
        assert_eq!(parse_wired_limit_mb(""), None);
        assert_eq!(parse_wired_limit_mb("99999999999999999999999"), None);
    }

    #[test]
    fn pmset() {
        assert_eq!(
            parse_pmset_batt("Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1) 80%"),
            Some(true)
        );
        assert_eq!(
            parse_pmset_batt("Now drawing from 'AC Power'\n -InternalBattery-0 100%; charged"),
            Some(false)
        );
        assert_eq!(parse_pmset_batt(""), None);
        assert_eq!(parse_pmset_batt("garbage"), None);
    }

    #[test]
    fn nvidia() {
        let gpus = parse_nvidia_smi("NVIDIA GeForce RTX 4090, 24564\nNVIDIA A100, 81920\n");
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 4090");
        assert_eq!(gpus[0].vram_bytes, Some(24564 * MIB));
        assert_eq!(gpus[1].vram_bytes, Some(81920 * MIB));
    }

    #[test]
    fn nvidia_bad_input() {
        assert!(parse_nvidia_smi("").is_empty());
        assert!(parse_nvidia_smi("No devices were found\n").is_empty());
        assert!(parse_nvidia_smi(", 1024\n").is_empty());
        let gpus = parse_nvidia_smi("RTX 3060, [N/A]\n");
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].vram_bytes, None);
    }

    fn ps(kind: &str, status: Option<&str>, online: Option<bool>) -> PowerSupply {
        PowerSupply {
            kind: kind.to_owned(),
            status: status.map(str::to_owned),
            online,
        }
    }

    #[test]
    fn power_supply() {
        assert_eq!(
            on_battery_from_power_supplies(&[
                ps("Mains", None, Some(true)),
                ps("Battery", Some("Charging"), None)
            ]),
            Some(false)
        );
        assert_eq!(
            on_battery_from_power_supplies(&[
                ps("Mains", None, Some(false)),
                ps("Battery", Some("Discharging"), None)
            ]),
            Some(true)
        );
        assert_eq!(on_battery_from_power_supplies(&[]), None);
        assert_eq!(
            on_battery_from_power_supplies(&[ps("Battery", Some("Full"), None)]),
            None
        );
    }
}
