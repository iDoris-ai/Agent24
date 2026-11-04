//! Hyphae `contract` handshake and `health` polling (ADR-038 D3, D5, §7.2, §7.3).
//!
//! `hyphae health` is read-only. ADR-038 §7.3: `checks.keystore` is metadata only —
//! no password, no file creation, and it does not lock the keystore. The long-running
//! daemon process holds the keystore itself and does not take this crate's
//! [`crate::keystore_lock::KeystoreWriteLock`], so a health probe must not take it
//! either: that lock only serializes *our* keystore writers, and holding it across a
//! relay probe (up to several seconds) would stall them without coordinating with the
//! daemon. Both commands go through [`HyphaeRunner::run`] with no `--password-stdin`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Notify;

use crate::runner::{Envelope, HyphaeRunner, Invocation};

/// How often to call `hyphae health` while the daemon is `running`.
///
/// Hyphae's own relay probe times out at 3s and the whole `health` command caps at
/// 5s (ADR-038 §7.3). On a lossy link about one probe in eight times out while the
/// relay is fine, so samples must not overlap and one timeout must not move the
/// component. 45s sits in the 30–60s window: three failures (the degrade threshold)
/// surface in a little over two minutes, and the client timeout always ends before
/// the next tick.
pub const HEALTH_INTERVAL: Duration = Duration::from_secs(45);

/// Outer bound for one `hyphae health` invocation. Hyphae caps the command at 5s;
/// 8s covers process startup and still leaves room for the runner's kill (SIGTERM,
/// then SIGKILL) so a stuck child cannot outlive the decision. The probe runs on
/// its own task — it must not block the daemon actor.
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(8);

/// Outer bound for one `hyphae contract` invocation. The command is a local JSON
/// dump; 5s is enough to notice a wedged binary and still refuse to spawn.
pub const CONTRACT_TIMEOUT: Duration = Duration::from_secs(5);

/// Consecutive failed samples before D5 `ready → degraded` (lock-configurable,
/// default 3). One successful sample returns to `ready`.
pub const HEALTH_FAILURES_BEFORE_DEGRADED: u32 = 3;

/// What the embedded lock says Agent24 will accept (ADR-038 D3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractExpectation {
    pub name: String,
    pub major: u64,
    pub min_minor: u64,
    pub tested: String,
}

/// Production arm: lock expectation, [`HEALTH_INTERVAL`], [`HEALTH_TIMEOUT`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeArm {
    pub expectation: ContractExpectation,
    pub health_interval: Duration,
    pub health_timeout: Duration,
    pub contract_timeout: Duration,
    /// Consecutive failed samples before `ready -> degraded` (lock-configurable).
    pub failures_before_degraded: u32,
}

impl ProbeArm {
    #[must_use]
    pub fn production(expectation: ContractExpectation) -> Self {
        Self {
            expectation,
            health_interval: HEALTH_INTERVAL,
            health_timeout: HEALTH_TIMEOUT,
            contract_timeout: CONTRACT_TIMEOUT,
            failures_before_degraded: HEALTH_FAILURES_BEFORE_DEGRADED,
        }
    }

    /// Replace the degrade threshold (a lock value). Zero is treated as one:
    /// a threshold that can never be met would hide every failure.
    #[must_use]
    pub fn with_failures_before_degraded(mut self, n: u32) -> Self {
        self.failures_before_degraded = n.max(1);
        self
    }
}

/// D3 outcome remembered for `GET /api/v1/components`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractView {
    /// No handshake has run (fixtures that never armed a probe).
    Unknown,
    Compatible,
    /// Name, major, or minor disagreed with the lock. The daemon was not spawned.
    Mismatch,
    /// `hyphae contract` did not return a readable envelope (timeout, crash, bad JSON).
    CheckFailed,
}

/// One row in the host health summary. `relays` and `keystore` are probes.
/// `daemon` is the optional `running` flag: shown, never a strike. `health`
/// is the row used when the command itself did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckView {
    pub ok: bool,
    pub consecutive_failures: u32,
}

impl Default for CheckView {
    fn default() -> Self {
        Self {
            ok: true,
            consecutive_failures: 0,
        }
    }
}

/// Latest health book. Host view only; the redacted component view never sees it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HealthView {
    /// `true` once a sample of this run reported `status:ok` (ADR-038 D5: the
    /// first passing probe is what takes `starting` to `ready`). Never cleared
    /// by a later failure: after that, the failure rule decides `degraded`.
    pub confirmed: bool,
    pub consecutive_failures: u32,
    pub degraded: bool,
    /// Probe names that failed on the sample which last moved the counter. Sorted.
    pub failed_probes: Vec<String>,
    pub checks: BTreeMap<String, CheckView>,
}

/// What [`crate::daemon::HyphaeDaemonSupervisor::probe_snapshot`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeSnapshot {
    /// `true` once a production (or test) arm exists: the health task is, or will
    /// be, probing. Without it `health` stays `None` forever and a running
    /// process cannot be held in `starting`.
    pub health_armed: bool,
    pub contract: ContractView,
    pub above_tested: bool,
    pub health: Option<HealthView>,
}

impl Default for ProbeSnapshot {
    fn default() -> Self {
        Self {
            health_armed: false,
            contract: ContractView::Unknown,
            above_tested: false,
            health: None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ProbeHub {
    inner: Arc<ProbeInner>,
}

struct ProbeInner {
    control: Mutex<Option<ProbeArm>>,
    state: Mutex<ProbeSnapshot>,
    notify: Notify,
}

impl ProbeHub {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(ProbeInner {
                control: Mutex::new(None),
                state: Mutex::new(ProbeSnapshot::default()),
                notify: Notify::new(),
            }),
        }
    }

    pub(crate) fn arm(&self, arm: ProbeArm) {
        *lock(&self.inner.control) = Some(arm);
        self.inner.notify.notify_waiters();
    }

    pub(crate) fn snapshot(&self) -> ProbeSnapshot {
        let armed = lock(&self.inner.control).is_some();
        let mut snapshot = lock(&self.inner.state).clone();
        snapshot.health_armed = armed;
        snapshot
    }

    fn failure_threshold(&self) -> u32 {
        lock(&self.inner.control)
            .as_ref()
            .map_or(HEALTH_FAILURES_BEFORE_DEGRADED, |arm| {
                arm.failures_before_degraded.max(1)
            })
    }

    pub(crate) fn expectation(&self) -> Option<ContractExpectation> {
        lock(&self.inner.control)
            .as_ref()
            .map(|arm| arm.expectation.clone())
    }

    pub(crate) fn contract_timeout(&self) -> Duration {
        lock(&self.inner.control)
            .as_ref()
            .map(|arm| arm.contract_timeout)
            .unwrap_or(CONTRACT_TIMEOUT)
    }

    /// `None` until armed: the health task waits instead of probing.
    pub(crate) fn health_timing(&self) -> Option<(Duration, Duration)> {
        lock(&self.inner.control)
            .as_ref()
            .map(|arm| (arm.health_interval, arm.health_timeout))
    }

    pub(crate) async fn notified(&self) {
        self.inner.notify.notified().await;
    }

    /// `true` when the reported contract may run. Logs (and only logs) when the
    /// minor is newer than `tested`.
    pub(crate) fn record_compatible(
        &self,
        expected: &ContractExpectation,
        name: &str,
        version: &str,
    ) -> bool {
        let mut state = lock(&self.inner.state);
        record_contract(&mut state, expected, name, version)
    }

    pub(crate) fn mark_check_failed(&self) {
        let mut state = lock(&self.inner.state);
        state.contract = ContractView::CheckFailed;
        state.above_tested = false;
    }

    pub(crate) fn reset_health(&self) {
        lock(&self.inner.state).health = None;
    }

    pub(crate) fn observe_health(&self, sample: HealthSample) {
        let threshold = self.failure_threshold();
        let mut state = lock(&self.inner.state);
        let health = state.health.get_or_insert_with(HealthView::default);
        observe_health(health, sample, threshold);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Apply one contract report. `false` means incompatible: do not spawn.
pub(crate) fn record_contract(
    snapshot: &mut ProbeSnapshot,
    expected: &ContractExpectation,
    name: &str,
    version: &str,
) -> bool {
    match judge_contract(expected, name, version) {
        ContractVerdict::Mismatch => {
            snapshot.contract = ContractView::Mismatch;
            snapshot.above_tested = false;
            false
        }
        ContractVerdict::Compatible { above_tested } => {
            snapshot.contract = ContractView::Compatible;
            snapshot.above_tested = above_tested;
            if above_tested {
                // Minor additions are allowed (D3). `tested` is what we have
                // exercised, not a ceiling, so this is a log line only.
                tracing::warn!(
                    tested = %expected.tested,
                    actual = %version,
                    "hyphae contract minor is newer than the tested version; accepting it"
                );
            }
            true
        }
    }
}

enum ContractVerdict {
    Compatible { above_tested: bool },
    Mismatch,
}

fn judge_contract(expected: &ContractExpectation, name: &str, version: &str) -> ContractVerdict {
    let Some((major, minor)) = parse_major_minor(version) else {
        return ContractVerdict::Mismatch;
    };
    if name != expected.name || major != expected.major || minor < expected.min_minor {
        return ContractVerdict::Mismatch;
    }
    let above_tested = parse_major_minor(&expected.tested)
        .is_some_and(|(tested_major, tested_minor)| major == tested_major && minor > tested_minor);
    ContractVerdict::Compatible { above_tested }
}

/// `major.minor` or `major.minor.patch`. No pre-release, no leading zeros.
fn parse_major_minor(version: &str) -> Option<(u64, u64)> {
    let mut parts = version.split('.');
    let major = parse_component(parts.next()?)?;
    let minor = parse_component(parts.next()?)?;
    match parts.next() {
        None => Some((major, minor)),
        Some(patch) => {
            parse_component(patch)?;
            if parts.next().is_some() {
                return None;
            }
            Some((major, minor))
        }
    }
}

fn parse_component(text: &str) -> Option<u64> {
    if text.is_empty() || (text.len() > 1 && text.starts_with('0')) {
        return None;
    }
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[derive(Debug, Deserialize)]
struct ContractData {
    name: String,
    contract_version: String,
}

pub(crate) async fn read_contract(
    runner: &HyphaeRunner,
    timeout: Duration,
) -> Result<(String, String), String> {
    let data = run_subcommand(runner, &["contract"], timeout).await?;
    let parsed: ContractData = serde_json::from_value(data)
        .map_err(|_| "contract envelope is missing name or contract_version".to_owned())?;
    if parsed.name.is_empty() || parsed.contract_version.is_empty() {
        return Err("contract envelope has an empty name or version".to_owned());
    }
    Ok((parsed.name, parsed.contract_version))
}

/// One finished `hyphae health` sample.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HealthSample {
    /// The command returned `ok:true` and a readable report. `success` is
    /// `status == ok` only (ADR-038 §7.3). `checks.daemon` is display data.
    Report {
        success: bool,
        checks: BTreeMap<String, bool>,
    },
    /// The process failed, timed out, or did not produce a report. Counts as
    /// one failure named `health`. Not a D5 `failed` (the daemon may still be up).
    Unrun,
}

pub(crate) async fn probe_health_with_timeout(
    runner: &HyphaeRunner,
    timeout: Duration,
) -> HealthSample {
    match run_subcommand(runner, &["health"], timeout).await {
        Ok(data) => parse_health(&data),
        Err(_) => HealthSample::Unrun,
    }
}

fn parse_health(data: &Value) -> HealthSample {
    let Some(status) = data.get("status").and_then(Value::as_str) else {
        return HealthSample::Unrun;
    };
    if !matches!(status, "ok" | "degraded" | "failed") {
        return HealthSample::Unrun;
    }
    let checks = checks_of(data);
    // ADR-038 §7.3: `status:ok` is one success; `degraded` or `failed` is one
    // failure. `checks.daemon.running` is not part of that vote and is not a
    // named failure. Whether the process is stopped is the supervisor's phase.
    // The flag stays in `checks` so the host view can show it.
    HealthSample::Report {
        success: status == "ok",
        checks,
    }
}

fn checks_of(data: &Value) -> BTreeMap<String, bool> {
    let Some(obj) = data.get("checks").and_then(Value::as_object) else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for (name, value) in obj {
        let Some(obj) = value.as_object() else {
            continue;
        };
        if let Some(ok) = obj.get("ok").and_then(Value::as_bool) {
            out.insert(name.clone(), ok);
        } else if let Some(running) = obj.get("running").and_then(Value::as_bool) {
            out.insert(name.clone(), running);
        }
    }
    out
}

fn observe_health(health: &mut HealthView, sample: HealthSample, threshold: u32) {
    match sample {
        HealthSample::Unrun => {
            let entry = health.checks.entry("health".to_owned()).or_default();
            entry.ok = false;
            entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
            note_failure(health, vec!["health".to_owned()], threshold);
        }
        HealthSample::Report { success, checks } => {
            let mut failed = Vec::new();
            for (name, ok) in &checks {
                let entry = health.checks.entry(name.clone()).or_default();
                entry.ok = *ok;
                // `daemon.running` is display only. It must not accumulate a
                // strike or land in `failed_probes` (ADR-038 §7.3).
                if *ok || name == "daemon" {
                    entry.consecutive_failures = 0;
                } else {
                    entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
                    failed.push(name.clone());
                }
            }
            // A real report means the command itself ran. Drop a previous
            // synthetic `health` miss unless this report has that check.
            if !checks.contains_key("health")
                && let Some(entry) = health.checks.get_mut("health")
            {
                entry.ok = true;
                entry.consecutive_failures = 0;
            }
            if success {
                // A check the report omitted did not fail this time. Leaving
                // the old `ok:false` would show a strike the sample cleared.
                for (name, entry) in health.checks.iter_mut() {
                    if !checks.contains_key(name) {
                        entry.ok = true;
                        entry.consecutive_failures = 0;
                    }
                }
                health.confirmed = true;
                health.consecutive_failures = 0;
                health.degraded = false;
                health.failed_probes.clear();
            } else {
                note_failure(health, failed, threshold);
            }
        }
    }
}

fn note_failure(health: &mut HealthView, mut names: Vec<String>, threshold: u32) {
    if names.is_empty() {
        names.push("health".to_owned());
    }
    names.sort();
    names.dedup();
    health.consecutive_failures = health.consecutive_failures.saturating_add(1);
    health.failed_probes = names;
    if health.consecutive_failures >= threshold {
        health.degraded = true;
    }
}

/// `hyphae <args>` with a timeout, no password, and no keystore lock.
async fn run_subcommand(
    runner: &HyphaeRunner,
    args: &[&str],
    timeout: Duration,
) -> Result<Value, String> {
    let inv = Invocation {
        args: args.iter().map(|arg| (*arg).into()).collect(),
        password: None,
        timeout: Some(timeout),
    };
    match runner.run(inv).await {
        Ok(Envelope::Ok { data }) => Ok(data),
        Ok(Envelope::Failed { error, message, .. }) => Err(format!("{error}: {message}")),
        Err(error) => Err(error.to_string()),
    }
}

/// Polls `hyphae health` while `running` is true. Dropping `running`'s sender
/// (the actor exited) ends the loop. A probe in flight is dropped with the
/// branch, and the runner's `kill_on_drop` reaps that child — the daemon actor
/// never awaits this function.
pub(crate) async fn health_loop(
    runner: Arc<HyphaeRunner>,
    hub: ProbeHub,
    mut running: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        if !*running.borrow_and_update() {
            if running.changed().await.is_err() {
                return;
            }
            continue;
        }
        // New generation: the previous run's strikes do not carry over.
        hub.reset_health();
        while *running.borrow() {
            let Some((interval, timeout)) = hub.health_timing() else {
                tokio::select! {
                    result = running.changed() => {
                        if result.is_err() {
                            return;
                        }
                    }
                    () = hub.notified() => {}
                }
                if !*running.borrow() {
                    break;
                }
                continue;
            };
            // `biased`: a `running` change that is ready together with a sample
            // wins, so a sample from the previous generation is never written
            // into the book `spawn_and_record` just cleared for the new one.
            tokio::select! {
                biased;
                result = running.changed() => {
                    if result.is_err() {
                        return;
                    }
                    // Left `running` mid-probe. Dropping the probe kills its child.
                    break;
                }
                sample = probe_health_with_timeout(&runner, timeout) => {
                    hub.observe_health(sample);
                }
            }
            if !*running.borrow() {
                break;
            }
            tokio::select! {
                result = running.changed() => {
                    if result.is_err() {
                        return;
                    }
                }
                () = tokio::time::sleep(interval) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::binary::{VerifiedBinary, sha256_of};
    use std::path::Path;
    use std::sync::Mutex as StdMutex;
    use std::time::Instant;

    fn expected() -> ContractExpectation {
        ContractExpectation {
            name: "hyphae-cli".into(),
            major: 1,
            min_minor: 0,
            tested: "1.0".into(),
        }
    }

    fn relay_failure() -> HealthSample {
        let mut checks = BTreeMap::new();
        checks.insert("keystore".into(), true);
        checks.insert("relays".into(), false);
        HealthSample::Report {
            success: false,
            checks,
        }
    }

    fn all_ok() -> HealthSample {
        let mut checks = BTreeMap::new();
        checks.insert("keystore".into(), true);
        checks.insert("relays".into(), true);
        HealthSample::Report {
            success: true,
            checks,
        }
    }

    #[test]
    fn threshold_comes_from_the_arm_and_zero_means_one() {
        assert_eq!(
            ProbeArm::production(expected()).failures_before_degraded,
            HEALTH_FAILURES_BEFORE_DEGRADED
        );
        let arm = ProbeArm::production(expected()).with_failures_before_degraded(0);
        assert_eq!(arm.failures_before_degraded, 1);
        let hub = ProbeHub::new();
        hub.arm(ProbeArm::production(expected()).with_failures_before_degraded(2));
        hub.observe_health(relay_failure());
        assert!(!hub.snapshot().health.unwrap().degraded);
        hub.observe_health(relay_failure());
        assert!(
            hub.snapshot().health.unwrap().degraded,
            "second failure of two"
        );
    }

    #[test]
    fn first_success_confirms_and_a_later_failure_does_not_unconfirm() {
        let hub = ProbeHub::new();
        assert!(!hub.snapshot().health_armed);
        hub.arm(ProbeArm::production(expected()));
        assert!(hub.snapshot().health_armed);
        hub.observe_health(relay_failure());
        assert!(!hub.snapshot().health.unwrap().confirmed);
        hub.observe_health(all_ok());
        assert!(hub.snapshot().health.unwrap().confirmed);
        hub.observe_health(relay_failure());
        assert!(hub.snapshot().health.unwrap().confirmed);
        hub.reset_health();
        assert!(hub.snapshot().health.is_none());
    }

    #[test]
    fn health_period_and_threshold_match_the_adr_defaults() {
        assert!(
            (Duration::from_secs(30)..=Duration::from_secs(60)).contains(&HEALTH_INTERVAL),
            "health poll must stay inside 30–60s, got {HEALTH_INTERVAL:?}"
        );
        assert_eq!(HEALTH_FAILURES_BEFORE_DEGRADED, 3);
        // Hyphae caps `health` at 5s (§7.3). Our bound has to be at least that,
        // and short enough that a stuck child cannot sit there for a minute.
        assert!(HEALTH_TIMEOUT >= Duration::from_secs(5));
        assert!(HEALTH_TIMEOUT <= Duration::from_secs(15));
        let arm = ProbeArm::production(expected());
        assert_eq!(arm.health_interval, HEALTH_INTERVAL);
        assert_eq!(arm.health_timeout, HEALTH_TIMEOUT);
        assert_eq!(arm.contract_timeout, CONTRACT_TIMEOUT);
    }

    #[test]
    fn one_failed_sample_is_not_degraded_and_the_nth_is_and_one_success_clears_it() {
        let mut health = HealthView::default();
        observe_health(
            &mut health,
            relay_failure(),
            HEALTH_FAILURES_BEFORE_DEGRADED,
        );
        assert!(!health.degraded, "a single relay failure must not degrade");
        assert_eq!(health.consecutive_failures, 1);
        assert_eq!(health.failed_probes, vec!["relays".to_owned()]);
        assert_eq!(health.checks["relays"].consecutive_failures, 1);
        assert_eq!(health.checks["keystore"].consecutive_failures, 0);

        for n in 2..HEALTH_FAILURES_BEFORE_DEGRADED {
            observe_health(
                &mut health,
                relay_failure(),
                HEALTH_FAILURES_BEFORE_DEGRADED,
            );
            assert!(!health.degraded, "failure {n} is still under the threshold");
        }
        observe_health(
            &mut health,
            relay_failure(),
            HEALTH_FAILURES_BEFORE_DEGRADED,
        );
        assert!(health.degraded);
        assert_eq!(health.consecutive_failures, HEALTH_FAILURES_BEFORE_DEGRADED);
        assert_eq!(health.failed_probes, vec!["relays".to_owned()]);
        assert!(health.checks["relays"].consecutive_failures >= HEALTH_FAILURES_BEFORE_DEGRADED);

        observe_health(&mut health, all_ok(), HEALTH_FAILURES_BEFORE_DEGRADED);
        assert!(!health.degraded, "one success returns to ready");
        assert_eq!(health.consecutive_failures, 0);
        assert!(health.failed_probes.is_empty());
        assert!(health.checks["relays"].ok);
        assert_eq!(health.checks["relays"].consecutive_failures, 0);
    }

    #[test]
    fn a_health_command_failure_counts_as_one_strike_named_health() {
        let mut health = HealthView::default();
        observe_health(
            &mut health,
            HealthSample::Unrun,
            HEALTH_FAILURES_BEFORE_DEGRADED,
        );
        assert!(!health.degraded);
        assert_eq!(health.consecutive_failures, 1);
        assert_eq!(health.failed_probes, vec!["health".to_owned()]);
        assert_eq!(health.checks["health"].consecutive_failures, 1);

        observe_health(&mut health, all_ok(), HEALTH_FAILURES_BEFORE_DEGRADED);
        assert_eq!(health.consecutive_failures, 0);
        assert!(health.checks["health"].ok);
        assert_eq!(health.checks["health"].consecutive_failures, 0);
    }

    #[test]
    fn daemon_running_false_is_display_only_and_does_not_decide_the_sample() {
        let mut checks = BTreeMap::new();
        checks.insert("daemon".into(), false);
        checks.insert("relays".into(), true);
        let sample = parse_health(&serde_json::json!({
            "status": "ok",
            "checks": {
                "daemon": {"running": false},
                "relays": {"ok": true},
                "extra": {"ignored": true},
                "note": "x"
            },
            "future": 1
        }));
        assert_eq!(
            sample,
            HealthSample::Report {
                success: true,
                checks
            }
        );

        // `status:failed` is still one failure, but the name is not `daemon`.
        let sample = parse_health(&serde_json::json!({
            "status": "failed",
            "checks": {"daemon": {"running": false}}
        }));
        let mut health = HealthView::default();
        observe_health(&mut health, sample, HEALTH_FAILURES_BEFORE_DEGRADED);
        assert!(!health.degraded);
        assert!(!health.failed_probes.iter().any(|name| name == "daemon"));
        assert!(!health.checks["daemon"].ok);
        assert_eq!(health.checks["daemon"].consecutive_failures, 0);
    }

    #[test]
    fn daemon_running_false_with_status_ok_stays_ready_after_three_samples() {
        let mut health = HealthView::default();
        for _ in 0..HEALTH_FAILURES_BEFORE_DEGRADED {
            let sample = parse_health(&serde_json::json!({
                "status": "ok",
                "checks": {
                    "daemon": {"running": false},
                    "relays": {"ok": true},
                    "keystore": {"ok": true}
                }
            }));
            observe_health(&mut health, sample, HEALTH_FAILURES_BEFORE_DEGRADED);
        }
        assert!(
            !health.degraded,
            "daemon.running=false must not turn a status:ok sample into a failure"
        );
        assert_eq!(health.consecutive_failures, 0);
        assert!(health.failed_probes.is_empty());
        assert!(!health.checks["daemon"].ok);
        assert_eq!(health.checks["daemon"].consecutive_failures, 0);
        assert!(health.checks["relays"].ok);
    }

    #[test]
    fn a_successful_sample_clears_checks_it_does_not_mention() {
        let mut health = HealthView::default();
        let mut checks = BTreeMap::new();
        checks.insert("relays".into(), false);
        checks.insert("daemon".into(), false);
        observe_health(
            &mut health,
            HealthSample::Report {
                success: false,
                checks,
            },
            HEALTH_FAILURES_BEFORE_DEGRADED,
        );
        assert!(!health.checks["relays"].ok);
        assert!(health.checks["relays"].consecutive_failures >= 1);
        assert!(!health.checks["daemon"].ok);

        let mut checks = BTreeMap::new();
        checks.insert("keystore".into(), true);
        observe_health(
            &mut health,
            HealthSample::Report {
                success: true,
                checks,
            },
            HEALTH_FAILURES_BEFORE_DEGRADED,
        );
        assert!(
            health.checks["relays"].ok && health.checks["relays"].consecutive_failures == 0,
            "an omitted relays row must not keep the old strike: {:?}",
            health.checks["relays"]
        );
        assert!(health.checks["daemon"].ok);
        assert_eq!(health.checks["daemon"].consecutive_failures, 0);
        assert!(health.checks["keystore"].ok);
        assert!(!health.degraded);
        assert_eq!(health.consecutive_failures, 0);
    }

    #[test]
    fn contract_mismatch_rejects_and_a_newer_minor_only_logs() {
        let expected = expected();
        let mut snapshot = ProbeSnapshot::default();
        assert!(
            !record_contract(&mut snapshot, &expected, "other-cli", "1.0"),
            "name"
        );
        assert_eq!(snapshot.contract, ContractView::Mismatch);

        assert!(!record_contract(
            &mut snapshot,
            &expected,
            "hyphae-cli",
            "2.0"
        ));
        assert!(!record_contract(
            &mut snapshot,
            &expected,
            "hyphae-cli",
            "0.9"
        ));
        assert!(!record_contract(
            &mut snapshot,
            &expected,
            "hyphae-cli",
            "1"
        ));
        assert!(!record_contract(
            &mut snapshot,
            &expected,
            "hyphae-cli",
            "1.0.0-dev"
        ));

        let lines = capture_warnings(|| {
            let mut snapshot = ProbeSnapshot::default();
            assert!(record_contract(
                &mut snapshot,
                &expected,
                "hyphae-cli",
                "1.0"
            ));
            assert!(!snapshot.above_tested);
            assert_eq!(snapshot.contract, ContractView::Compatible);
            assert!(record_contract(
                &mut snapshot,
                &expected,
                "hyphae-cli",
                "1.0.4"
            ));
            assert!(
                !snapshot.above_tested,
                "a higher patch is not a higher minor"
            );
        });
        assert!(
            lines.is_empty(),
            "an accepted tested minor must not warn: {lines:?}"
        );

        let lines = capture_warnings(|| {
            let mut snapshot = ProbeSnapshot::default();
            assert!(record_contract(
                &mut snapshot,
                &expected,
                "hyphae-cli",
                "1.4"
            ));
            assert!(snapshot.above_tested);
            assert_eq!(snapshot.contract, ContractView::Compatible);
            assert!(
                !record_contract(&mut snapshot, &expected, "hyphae-cli", "9.0"),
                "a mismatch must not be reported as compatible"
            );
        });
        assert!(
            lines
                .iter()
                .any(|line| line.contains("newer than the tested")
                    && line.contains("1.4")
                    && line.contains("1.0")),
            "a newer minor must be logged and still accepted: {lines:?}"
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("newer than the tested"))
                .count(),
            1,
            "the mismatch arm must not emit the newer-minor log"
        );
    }

    #[test]
    fn unknown_contract_fields_are_ignored() {
        let value = serde_json::json!({
            "name": "hyphae-cli",
            "contract_version": "1.0",
            "binary_version": "dev",
            "provides": ["nostr-dm"],
            "future": {"kept": true}
        });
        let parsed: ContractData = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.name, "hyphae-cli");
        assert_eq!(parsed.contract_version, "1.0");
    }

    #[tokio::test]
    async fn health_timeout_is_one_failure_and_does_not_take_the_keystore_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = install_script(
            tmp.path(),
            r#"#!/bin/sh
case "$1" in
  health) sleep 30 ;;
esac
exit 0
"#,
        )
        .await;
        let started = Instant::now();
        let sample = probe_health_with_timeout(&runner, Duration::from_millis(200)).await;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "health must return on its timeout, elapsed {:?}",
            started.elapsed()
        );
        assert_eq!(sample, HealthSample::Unrun);
        let mut health = HealthView::default();
        observe_health(&mut health, sample, HEALTH_FAILURES_BEFORE_DEGRADED);
        assert!(!health.degraded);
        assert_eq!(health.consecutive_failures, 1);

        let tmp = tempfile::tempdir().unwrap();
        let runner = install_script(
            tmp.path(),
            r#"#!/bin/sh
echo '{"ok":true,"data":{"status":"ok","checks":{"relays":{"ok":true}}}}'
exit 0
"#,
        )
        .await;
        let lock = runner.keystore_lock().clone();
        let guard = lock.acquire().await;
        let probe = tokio::spawn(async move {
            probe_health_with_timeout(&runner, Duration::from_secs(2)).await
        });
        // The guard stays held until this wait ends. A probe that took
        // `KeystoreWriteLock` could not finish inside it; five seconds only
        // covers a slow spawn while the suite is loaded.
        let finished = tokio::time::timeout(Duration::from_secs(5), probe).await;
        drop(guard);
        let sample = finished
            .expect("health waited on the keystore lock")
            .unwrap();
        assert!(matches!(sample, HealthSample::Report { success: true, .. }));
    }

    #[tokio::test]
    async fn an_empty_contract_name_is_a_failed_check_not_a_parsed_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = install_script(
            tmp.path(),
            r#"#!/bin/sh
printf '%s\n' '{"ok":true,"data":{"name":"","contract_version":"1.0"}}'
exit 0
"#,
        )
        .await;
        let err = read_contract(&runner, Duration::from_secs(2))
            .await
            .expect_err("an empty name is not a contract we can judge");
        assert!(err.contains("empty"), "{err}");
    }

    async fn install_script(dir: &Path, script: &str) -> HyphaeRunner {
        let source = dir.join("hyphae-fake.sh");
        tokio::fs::write(&source, script).await.unwrap();
        let bytes = tokio::fs::read(&source).await.unwrap();
        let bin = VerifiedBinary::install(&source, sha256_of(&bytes), &dir.join("bin"))
            .await
            .unwrap();
        let home = dir.join("home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        HyphaeRunner::new(bin, home, Duration::from_secs(5))
    }

    fn capture_warnings(body: impl FnOnce()) -> Vec<String> {
        let lines = Arc::new(StdMutex::new(Vec::new()));
        let subscriber = Capture(Arc::clone(&lines));
        tracing::subscriber::with_default(subscriber, body);
        lines.lock().unwrap().clone()
    }

    struct Capture(Arc<StdMutex<Vec<String>>>);

    impl tracing::Subscriber for Capture {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::WARN
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut message = String::new();
            event.record(&mut Recorder(&mut message));
            self.0.lock().unwrap().push(message);
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    struct Recorder<'a>(&'a mut String);

    impl tracing::field::Visit for Recorder<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write as _;
            let _ = write!(self.0, "{}={value:?} ", field.name());
        }
    }
}
