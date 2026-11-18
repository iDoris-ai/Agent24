//! Kernel-capability consent for an out-of-process OS package.
//!
//! The trust list is unchanged. This module only answers whether a package
//! that is already allowed to load skips the consent card, and what the
//! permission fingerprint of its `requires` declaration is.
//!
//! Exempt only when both hold: the module name is on the bundled list
//! (`documents`, `sin90`), and on macOS the signing team is the built-in
//! team. The list is by module name, not by how the package was installed.
//! Attach and MCP are not classified here.

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Modules that ship inside the desktop app. OSI-2 added `sin90`.
pub const BUNDLED_OS_MODULES: &[&str] = &["documents", "sin90"];

/// Host field on an OS package row.
/// `missing` means the card applies and there is no usable record.
/// `granted` means a record is still in force. CONSENT-1 only emitted the
/// first two; `granted` is how the desktop tells a consented module from
/// one it still has to ask about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelConsentClass {
    Exempt,
    Missing,
    Granted,
}

/// `exempt` when `name` is bundled and, on macOS, `team` is the built-in
/// Developer ID team. Anywhere else a bundled name is exempt: there is no
/// team to read. A missing team on macOS is not exempt.
pub fn classify(name: &str, team: Option<&str>, macos: bool) -> KernelConsentClass {
    if !BUNDLED_OS_MODULES.contains(&name) {
        return KernelConsentClass::Missing;
    }
    if !macos {
        return KernelConsentClass::Exempt;
    }
    if team == Some(agent24_os_proc::DEVELOPER_ID_TEAM_ID) {
        KernelConsentClass::Exempt
    } else {
        KernelConsentClass::Missing
    }
}

/// Sorted `requires.kernel` names, then sorted `requires.operations`.
/// The version is not part of it. The two sides stay labeled so a kernel
/// name cannot collide with an operation name.
pub fn fingerprint(kernel: &[String], operations: &[String]) -> String {
    let mut kernel: Vec<&str> = kernel.iter().map(String::as_str).collect();
    kernel.sort_unstable();
    kernel.dedup();
    let mut operations: Vec<&str> = operations.iter().map(String::as_str).collect();
    operations.sort_unstable();
    operations.dedup();
    format!("k={};o={}", kernel.join(","), operations.join(","))
}

/// One sentence per kernel capability (plan §2.3). The native dialog and
/// `agent24 os consent show` use these words. The desktop translates by id.
pub fn phrase(capability: &str) -> &'static str {
    match capability {
        "events" => "以这个模块的名字发出事件",
        "models" => "让内核替它调用模型",
        "scheduler" => "登记和查看定时任务",
        "memory" => "使用只属于它自己的那一份记忆",
        "approval" => "发起审批、查询审批结果",
        _ => "一项没有对应说明的内核能力",
    }
}

/// Declared names that the out-of-process ceiling will actually consider.
pub fn offered(kernel: &[String]) -> Vec<String> {
    let mut names: Vec<String> = kernel
        .iter()
        .filter_map(|name| agent24_domain::Capability::parse(name).ok())
        .filter(|cap| crate::capabilities::requires::KERNEL_OOP_GRANTS.contains(cap))
        .map(|cap| cap.as_str().to_owned())
        .collect();
    names.sort();
    names.dedup();
    names
}

pub fn intersect(offered: &[String], consented: &[String]) -> Vec<String> {
    offered
        .iter()
        .filter(|name| consented.iter().any(|agreed| agreed == *name))
        .cloned()
        .collect()
}

/// `SameOrShrink` keeps the record. Anything else, including a fingerprint
/// this build cannot read, drops it.
pub fn same_or_shrink(stored: &str, current: &str) -> bool {
    let Some((stored_kernel, stored_ops)) = split_fingerprint(stored) else {
        return false;
    };
    let Some((current_kernel, current_ops)) = split_fingerprint(current) else {
        return false;
    };
    current_kernel.is_subset(&stored_kernel) && current_ops.is_subset(&stored_ops)
}

fn split_fingerprint(fingerprint: &str) -> Option<(BTreeSet<String>, BTreeSet<String>)> {
    let (kernel, operations) = fingerprint.split_once(";o=")?;
    let kernel = kernel.strip_prefix("k=")?;
    if kernel.contains(';') || operations.contains(';') {
        return None;
    }
    Some((side(kernel), side(operations)))
}

fn side(text: &str) -> BTreeSet<String> {
    if text.is_empty() {
        BTreeSet::new()
    } else {
        text.split(',').map(str::to_owned).collect()
    }
}

pub fn expires_at(now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let days = max_days()?;
    Ok(now + chrono::Duration::days(i64::from(days)))
}

/// `A24_MODULE_GRANT_MAX_DAYS`, default 30, at most 90. The tool-grant
/// switch is not consulted.
pub fn max_days() -> Result<u32, String> {
    const CAP: u32 = crate::consent_routes::MAX_GRANT_DAYS_CAP;
    match std::env::var("A24_MODULE_GRANT_MAX_DAYS") {
        Err(_) => Ok(30),
        Ok(raw) => match raw.parse::<u32>() {
            Ok(days) if (1..=CAP).contains(&days) => Ok(days),
            _ => Err(format!(
                "A24_MODULE_GRANT_MAX_DAYS={raw:?} must be a number of days from 1 to {CAP}"
            )),
        },
    }
}

pub fn still_current(expires_at: &str, now: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(expires_at).is_ok_and(|expires| expires > now)
}

/// What a gated package may hold right now. `delete` means the stored row
/// no longer applies (grown, expired, or unreadable).
pub struct Held {
    pub class: KernelConsentClass,
    pub grants: Vec<String>,
    pub delete: bool,
}

pub fn hold(
    current_fingerprint: &str,
    offered: &[String],
    row: Option<&agent24_store::KernelConsentRow>,
    now: DateTime<Utc>,
) -> Held {
    let Some(row) = row else {
        return Held {
            class: KernelConsentClass::Missing,
            grants: Vec::new(),
            delete: false,
        };
    };
    if !still_current(&row.expires_at, now)
        || !same_or_shrink(&row.fingerprint, current_fingerprint)
    {
        return Held {
            class: KernelConsentClass::Missing,
            grants: Vec::new(),
            delete: true,
        };
    }
    Held {
        class: KernelConsentClass::Granted,
        grants: intersect(offered, &row.capabilities),
        delete: false,
    }
}

struct Live {
    caps: BTreeSet<String>,
    expires: DateTime<Utc>,
}

static LIVE: Mutex<Option<HashMap<String, Live>>> = Mutex::new(None);

fn live_map() -> std::sync::MutexGuard<'static, Option<HashMap<String, Live>>> {
    LIVE.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Remember what the next kernel call may do. An empty set denies every
/// capability. Exempt packages are removed so the mount snapshot stands.
pub fn track(name: &str, caps: impl IntoIterator<Item = impl AsRef<str>>, expires: DateTime<Utc>) {
    let caps = caps
        .into_iter()
        .map(|cap| cap.as_ref().to_owned())
        .collect();
    let mut map = live_map();
    map.get_or_insert_with(HashMap::new)
        .insert(name.to_owned(), Live { caps, expires });
}

pub fn untrack(name: &str) {
    if let Some(map) = live_map().as_mut() {
        map.remove(name);
    }
}

/// `true` when this module is tracked and the capability is not in force.
/// An untracked module keeps the snapshot the mount handed it.
pub fn live_denies(module: &str, capability: &str) -> bool {
    let map = live_map();
    match map.as_ref().and_then(|map| map.get(module)) {
        None => false,
        Some(live) => Utc::now() >= live.expires || !live.caps.contains(capability),
    }
}

/// `None`: do not narrow (no store in this process, or the package is exempt).
/// `Some`: only these names, which may be empty.
pub async fn narrow_for_mount(
    store: Option<&agent24_store::Store>,
    name: &str,
    team: Option<&str>,
    kernel: &[String],
) -> Option<BTreeSet<String>> {
    let store = store?;
    if classify(name, team, cfg!(target_os = "macos")) == KernelConsentClass::Exempt {
        untrack(name);
        return None;
    }
    let current = fingerprint(kernel, &[]);
    let offer = offered(kernel);
    let row = store.get_kernel_consent(name).await.ok().flatten();
    let now = Utc::now();
    let held = hold(&current, &offer, row.as_ref(), now);
    if held.delete {
        let _ = store.delete_kernel_consent(name).await;
    }
    let expires = row
        .as_ref()
        .and_then(|row| DateTime::parse_from_rfc3339(&row.expires_at).ok())
        .map(|expires| expires.with_timezone(&Utc))
        .filter(|expires| *expires > now && !held.delete)
        .unwrap_or_else(|| now + chrono::Duration::days(365));
    track(name, held.grants.iter(), expires);
    Some(held.grants.into_iter().collect())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn bundled_builtin_team_is_exempt_and_any_other_team_is_missing() {
        let builtin = agent24_os_proc::DEVELOPER_ID_TEAM_ID;
        assert_eq!(
            classify("documents", Some(builtin), true),
            KernelConsentClass::Exempt
        );
        assert_eq!(
            classify("sin90", Some(builtin), true),
            KernelConsentClass::Exempt
        );
        assert_eq!(
            classify("documents", Some("OTHERTEAM1"), true),
            KernelConsentClass::Missing
        );
        assert_eq!(
            classify("sin90", None, true),
            KernelConsentClass::Missing,
            "macOS with no team read is not exempt"
        );
        assert_eq!(
            classify("widget", Some(builtin), true),
            KernelConsentClass::Missing
        );
    }

    #[test]
    fn a_bundled_name_is_exempt_where_there_is_no_team_to_read() {
        assert_eq!(
            classify("documents", None, false),
            KernelConsentClass::Exempt
        );
        assert_eq!(classify("sin90", None, false), KernelConsentClass::Exempt);
        assert_eq!(classify("widget", None, false), KernelConsentClass::Missing);
    }

    #[test]
    fn the_fingerprint_ignores_version_and_changes_when_a_capability_is_added() {
        let events = fingerprint(&["events".into()], &[]);
        assert_eq!(events, fingerprint(&["events".into()], &[]));
        assert_ne!(
            events,
            fingerprint(&["events".into(), "models".into()], &[])
        );
        assert_eq!(
            fingerprint(&["models".into(), "events".into()], &[]),
            fingerprint(&["events".into(), "models".into()], &[])
        );
        assert_ne!(
            fingerprint(&["events".into()], &[]),
            fingerprint(&["events".into()], &["scheduler.upsert".into()])
        );
    }

    #[test]
    fn a_smaller_fingerprint_is_kept_and_a_larger_one_is_not() {
        let stored = fingerprint(&["events".into(), "models".into()], &[]);
        let same = fingerprint(&["models".into(), "events".into()], &[]);
        let smaller = fingerprint(&["events".into()], &[]);
        let larger = fingerprint(&["events".into(), "models".into(), "memory".into()], &[]);
        assert!(same_or_shrink(&stored, &same));
        assert!(same_or_shrink(&stored, &smaller));
        assert!(!same_or_shrink(&stored, &larger));
        assert!(!same_or_shrink("not-a-fingerprint", &same));
    }

    #[test]
    fn an_expired_or_grown_record_grants_nothing_and_a_shrink_drops_the_extra() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-18T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let offer = vec!["events".into(), "models".into()];
        let mut row = agent24_store::KernelConsentRow {
            module: "widget".into(),
            fingerprint: fingerprint(&offer, &[]),
            capabilities: offer.clone(),
            team_id: None,
            version: "1".into(),
            decided_at: "2026-11-01T00:00:00Z".into(),
            expires_at: "2026-11-01T00:00:00Z".into(),
        };
        let expired = hold(&row.fingerprint, &offer, Some(&row), now);
        assert!(expired.delete);
        assert!(expired.grants.is_empty());
        row.expires_at = "2026-12-18T00:00:00Z".into();
        let shrunk = fingerprint(&["events".into()], &[]);
        let kept = hold(&shrunk, &["events".into()], Some(&row), now);
        assert!(!kept.delete);
        assert_eq!(kept.grants, ["events"]);
        assert_eq!(kept.class, KernelConsentClass::Granted);
    }

    #[tokio::test]
    async fn a_deleted_row_narrows_the_next_mount_to_nothing() {
        let store = agent24_store::Store::open_memory().await.unwrap();
        let kernel = vec!["events".into(), "models".into()];
        store
            .upsert_kernel_consent(&agent24_store::KernelConsentRow {
                module: "widget".into(),
                fingerprint: fingerprint(&kernel, &[]),
                capabilities: kernel.clone(),
                team_id: Some("OTHERTEAM1".into()),
                version: "1".into(),
                decided_at: "2026-11-18T00:00:00Z".into(),
                expires_at: "2099-01-01T00:00:00Z".into(),
            })
            .await
            .unwrap();
        let held = narrow_for_mount(Some(&store), "widget", Some("OTHERTEAM1"), &kernel)
            .await
            .unwrap();
        assert!(held.contains("events") && held.contains("models"));
        store.delete_kernel_consent("widget").await.unwrap();
        let next = narrow_for_mount(Some(&store), "widget", Some("OTHERTEAM1"), &kernel)
            .await
            .unwrap();
        assert!(next.is_empty());
        let exempt = narrow_for_mount(
            Some(&store),
            "documents",
            Some(agent24_os_proc::DEVELOPER_ID_TEAM_ID),
            &["events".into()],
        )
        .await;
        assert!(exempt.is_none());
    }
}
