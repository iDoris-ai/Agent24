//! C9-A: one `requires` model and one resolver for the three kernel-grant sites.
//!
//! Lock files, `component.json`, and `domain-os.yml`'s `kernel_capabilities`
//! all become a [`RequiresSpec`]. [`resolve`] intersects that declaration with
//! the form's ceiling. This slice's consent is always [`Consent::FirstPartyEnabled`]
//! (enabling a first-party module is the consent, jason 2026-11-12), so the
//! granted set is the same intersection [`agent24_domain::Grants::granting`]
//! already computed. Reason codes are recorded for a later `/components` view;
//! an invalid declaration grants nothing and does not refuse the mount.

use std::collections::BTreeSet;
use std::fmt;

use agent24_domain::{Capability, DomainOsManifest, Grants};

use super::Operation;

/// What the kernel is willing to lend an in-process domain OS today. A module
/// may REQUEST more in its manifest; [`Grants::granting`] intersects, so
/// asking gains nothing. The list grows as `KernelCtx` gains handles —
/// `Models` and `Scheduler` are deliberately absent because there is nothing
/// to hand out yet, and granting a capability with no handle would be a lie.
///
/// `Memory` joined the list in F1, when `KernelCtx::memory` gained a handle to
/// give: a module that asks for it gets a view of the shared memory base scoped
/// to ITS OWN partition (see [`crate::os_memory`]). Before that handle existed,
/// two domain OSes under one user shared that base.
///
/// `Approval` joined in T7b/ME-3e, when `KernelCtx::approval` gained a handle
/// (`ApprovalRequester`, backed by `PolicyApprovalBackend` in `domain`).
pub(crate) const KERNEL_GRANTS: &[Capability] =
    &[Capability::Events, Capability::Memory, Capability::Approval];

/// What an out-of-process module may be granted. A SEPARATE list from
/// [`KERNEL_GRANTS`] on purpose — see `docs/design/T7a-ME3e-grants-and-events.md`
/// §1 for why this list exists at all. It used to be strictly narrower than
/// [`KERNEL_GRANTS`]; ME4-1.4.1 (`docs/design/ME4-S1-scheduler-callback.md`
/// §6.5) broke that: `Scheduler` is now WIDER here, because out-of-process
/// modules have a real handle (`CallbackDeps.scheduler`) that no in-process
/// module does. Each list only ever promises what its own side can actually
/// back with a handle — the two are independent for that reason, not because
/// one is meant to contain the other.
/// `Approval` joined in T7b/ME-3e, alongside the wire handlers in
/// `crate::approval_callback` that give it a real handler.
///
/// `Memory` joined in T8.5c-W-mount. Out-of-process access to the shared
/// memory base goes through the OOP wire surface (`_a24/memory/private/*`,
/// T8.5c-W-wire), gated by [`crate::os_memory::MemoryEntitlement`] rather
/// than by this list alone — a module may be in this grant set and still
/// receive `MemoryEntitlement::NONE` (its partition could not be recorded,
/// or the daemon has no connection-admission budget to offer — see
/// `MemoryLease::admission()`). This list only says the kernel is WILLING to
/// consider lending memory to an out-of-process module; whether it actually
/// can is decided per mount.
///
/// `Scheduler` joined in ME4-1.4.1 (`docs/design/ME4-S1-scheduler-callback.md`
/// §6.5): out-of-process modules now have a real handle
/// (`CallbackDeps.scheduler`) and a real handler surface
/// (`_a24/scheduler/{upsert,delete,list}`, `crate::scheduler_callback`).
/// Deliberately **not** added to [`KERNEL_GRANTS`] — there is still no
/// in-process scheduler handle to hand out, and this list's own doc comment
/// above says why that matters (`a_capability_the_kernel_cannot_serve_is_not_granted`
/// keeps that promise honest for in-process modules).
///
/// `Models` joined in ME4-4.2.2b2 (`docs/design/ME4-S2-model-callback.md`
/// §2.4): out-of-process modules may now hold a real handle
/// (`CallbackDeps.models: Option<ModelCallbackDeps>` — `Option` because a
/// daemon that built no router still runs) and a real handler surface
/// (`_a24/model/complete`, `crate::model_callback`). Same reasoning as
/// `Scheduler` for why it is not in [`KERNEL_GRANTS`]: `KernelCtx` has no
/// model handle to lend an in-process module.
pub(crate) const KERNEL_OOP_GRANTS: &[Capability] = &[
    Capability::Events,
    Capability::Approval,
    Capability::Memory,
    Capability::Scheduler,
    Capability::Models,
];

/// A3 P2 (design §1 "本期只用 `events`、`models`"): the only two capabilities
/// an attached module may be granted. Deliberately narrower than
/// [`KERNEL_OOP_GRANTS`] — `memory`/`scheduler`/`approval` have no attach-side
/// story yet (no partition-allocation pass runs for an attached module, and
/// `host_commands` — A3's own reverse channel — is not one of
/// [`Capability`]'s variants), so granting them here would be scope no
/// judgement or design section asks for. A manifest MAY still declare them
/// (`kernel_capabilities` is an open list at the manifest layer); the
/// resolver's intersection with this list is what refuses them, the same
/// "asking gains nothing" rule every other grant list in this daemon already
/// documents.
pub(crate) const ATTACH_OOP_GRANTS: &[Capability] = &[Capability::Events, Capability::Models];

/// Which of the three grant sites is asking. The ceilings differ on purpose:
/// each list only names capabilities that form can actually hand out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GrantForm {
    InProcess,
    OutOfProcess,
    Attach,
}

impl GrantForm {
    pub(crate) const fn ceiling(self) -> &'static [Capability] {
        match self {
            Self::InProcess => KERNEL_GRANTS,
            Self::OutOfProcess => KERNEL_OOP_GRANTS,
            Self::Attach => ATTACH_OOP_GRANTS,
        }
    }

    /// This slice's three forms all have a callback channel for every
    /// capability in the ceiling, so `no_channel` is empty and the grant
    /// set stays the intersection.
    pub(crate) const fn host_cap(self) -> HostCap<'static> {
        HostCap {
            ceiling: self.ceiling(),
            no_channel: &[],
        }
    }
}

/// Host side of [`resolve`]: the ceiling, plus capabilities inside that
/// ceiling that have no callback channel (`no_channel`, not granted).
#[derive(Clone, Copy, Debug)]
pub(crate) struct HostCap<'a> {
    pub ceiling: &'a [Capability],
    pub no_channel: &'a [Capability],
}

/// Consent input. Only the first-party rule exists in this slice; it does
/// not narrow the intersection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Consent {
    FirstPartyEnabled,
}

/// Where the module's first-party label came from. The enroll Team ID list
/// attests a process, not an OS package or an attach module (C9 plan §8.1),
/// so this slice records [`Self::Unknown`] and does not let the label change
/// the grant set. Later slices display it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModuleOrigin {
    Unknown,
}

impl fmt::Display for ModuleOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unknown => "unknown",
        })
    }
}

/// Closed denial reasons (C9-A).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DenyReason {
    NotDeclared,
    AboveCeiling,
    NoChannel,
    RequiresInvalid,
}

impl DenyReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NotDeclared => "not_declared",
            Self::AboveCeiling => "above_ceiling",
            Self::NoChannel => "no_channel",
            Self::RequiresInvalid => "requires_invalid",
        }
    }
}

impl fmt::Display for DenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Denial {
    pub item: String,
    pub reason: DenyReason,
}

/// Kernel capabilities plus canonical operations. `invalid` tokens make the
/// whole spec grant nothing ([`DenyReason::RequiresInvalid`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequiresSpec {
    kernel: Vec<Capability>,
    /// Canonical operations. No call site lends them in this slice.
    #[allow(dead_code)]
    operations: Vec<Operation>,
    invalid: Vec<String>,
}

impl RequiresSpec {
    /// A parsed manifest already rejected unknown kernel names. Operations
    /// are not a manifest field; they stay empty on this path.
    pub(crate) fn from_manifest(manifest: &DomainOsManifest) -> Self {
        Self {
            kernel: manifest.kernel_capabilities().to_vec(),
            operations: Vec::new(),
            invalid: Vec::new(),
        }
    }

    /// Lock / `component.json` strings. A kernel token must be a
    /// [`Capability`] snake_case name and not a `REGISTRY.md` capability id.
    /// An operation must be [`Operation::canonical`] (dotted), not a slash alias.
    pub(crate) fn parse<S: AsRef<str>>(kernel: &[S], operations: &[S]) -> Self {
        let mut invalid = Vec::new();
        let mut caps = Vec::new();
        let mut seen_caps = BTreeSet::new();
        for name in kernel {
            let name = name.as_ref();
            match Capability::parse(name) {
                Ok(cap) if !is_registry_capability_id(name) && seen_caps.insert(cap) => {
                    caps.push(cap);
                }
                Ok(_) if is_registry_capability_id(name) => invalid.push(name.to_owned()),
                Ok(_) => {}
                Err(_) => invalid.push(name.to_owned()),
            }
        }
        let mut ops = Vec::new();
        let mut seen_ops = BTreeSet::new();
        for name in operations {
            let name = name.as_ref();
            match Operation::canonical(name) {
                Some(op) if seen_ops.insert(op) => ops.push(op),
                Some(_) => {}
                None => invalid.push(name.to_owned()),
            }
        }
        if !invalid.is_empty() {
            caps.clear();
            ops.clear();
        }
        Self {
            kernel: caps,
            operations: ops,
            invalid,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.invalid.is_empty()
    }

    pub(crate) fn kernel(&self) -> &[Capability] {
        &self.kernel
    }
}

/// `requires.kernel` token: a real kernel capability, not a registry id.
pub(crate) fn kernel_token_ok(name: &str) -> bool {
    Capability::parse(name).is_ok() && !is_registry_capability_id(name)
}

/// `requires.operations` token: canonical dotted [`Operation`] name.
pub(crate) fn operation_token_ok(name: &str) -> bool {
    Operation::canonical(name).is_some()
}

fn is_registry_capability_id(name: &str) -> bool {
    crate::component_manifest::REGISTERED_PROVIDERS
        .iter()
        .any(|(id, _)| *id == name)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resolution {
    pub granted: Grants,
    pub denied: Vec<Denial>,
    /// Not an input to the grant set. Display-only, and unknown this slice.
    #[allow(dead_code)]
    pub origin: ModuleOrigin,
}

/// `declared ∩ host.ceiling`, minus capabilities the host has no channel for.
/// [`Consent::FirstPartyEnabled`] does not remove anything further.
/// An invalid declaration grants nothing (`requires_invalid`) and still
/// returns a resolution, so the caller keeps running the component.
pub(crate) fn resolve(declared: &RequiresSpec, host: HostCap<'_>, consent: Consent) -> Resolution {
    match consent {
        Consent::FirstPartyEnabled => {}
    }
    if !declared.is_valid() {
        return Resolution {
            granted: Grants::default(),
            denied: declared
                .invalid
                .iter()
                .map(|item| Denial {
                    item: item.clone(),
                    reason: DenyReason::RequiresInvalid,
                })
                .collect(),
            origin: ModuleOrigin::Unknown,
        };
    }
    let mut denied = Vec::new();
    let mut requested = Vec::new();
    let mut seen = BTreeSet::new();
    for cap in &declared.kernel {
        if !seen.insert(*cap) {
            continue;
        }
        if host.ceiling.contains(cap) && host.no_channel.contains(cap) {
            denied.push(Denial {
                item: cap.as_str().to_owned(),
                reason: DenyReason::NoChannel,
            });
        } else if !host.ceiling.contains(cap) {
            denied.push(Denial {
                item: cap.as_str().to_owned(),
                reason: DenyReason::AboveCeiling,
            });
        } else {
            requested.push(*cap);
        }
    }
    for cap in host.ceiling {
        if !seen.contains(cap) {
            denied.push(Denial {
                item: cap.as_str().to_owned(),
                reason: DenyReason::NotDeclared,
            });
        }
    }
    Resolution {
        granted: Grants::granting(&requested, host.ceiling),
        denied,
        origin: ModuleOrigin::Unknown,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const DOCUMENTS_YML: &str = include_str!("../../../agent24-documents/domain-os.yml");
    const DOCUMENTS_JSON: &str = include_str!("../../../agent24-documents/component.json");
    const NODE_MODULE_YML: &str = include_str!("../../../../../examples/node-module/domain-os.yml");
    const IDORIS_LOCK: &str = include_str!("../../../../crates/agent24-idoris/idoris.lock.json");
    const HYPHAE_LOCK: &str = include_str!("../../../../crates/agent24-comm/hyphae.lock.json");

    fn names(grants: &Grants) -> Vec<&'static str> {
        grants.iter().map(Capability::as_str).collect()
    }

    fn manifest(yaml: &str) -> DomainOsManifest {
        DomainOsManifest::from_yaml(yaml).unwrap()
    }

    fn agentear(caps: &str) -> DomainOsManifest {
        manifest(&format!(
            "name: agentear\nversion: \"1\"\nroute_namespace: /api/v1/agentear\n\
             event_module: agentear\ndata_dir: ~/.agent24/os/agentear/\n\
             impl_kind: attached_process\nkernel_capabilities: [{caps}]\n"
        ))
    }

    fn in_process_os(caps: &str) -> DomainOsManifest {
        manifest(&format!(
            "name: sin90\nversion: \"0.1.0\"\nroute_namespace: /api/v1/sin90\n\
             event_module: sin90\ndata_dir: ~/.agent24/os/sin90/\n\
             kernel_capabilities: [{caps}]\nimpl_kind: in_process_crate\n"
        ))
    }

    fn legacy(manifest: &DomainOsManifest, ceiling: &[Capability]) -> Grants {
        Grants::granting(manifest.kernel_capabilities(), ceiling)
    }

    fn resolved(manifest: &DomainOsManifest, form: GrantForm) -> Resolution {
        resolve(
            &RequiresSpec::from_manifest(manifest),
            form.host_cap(),
            Consent::FirstPartyEnabled,
        )
    }

    fn reason(resolution: &Resolution, item: &str) -> &'static str {
        resolution
            .denied
            .iter()
            .find(|d| d.item == item)
            .unwrap_or_else(|| panic!("no denial for {item}: {:?}", resolution.denied))
            .reason
            .as_str()
    }

    #[test]
    fn ceilings_keep_the_values_the_three_sites_used() {
        assert_eq!(
            KERNEL_GRANTS,
            &[Capability::Events, Capability::Memory, Capability::Approval]
        );
        assert_eq!(
            KERNEL_OOP_GRANTS,
            &[
                Capability::Events,
                Capability::Approval,
                Capability::Memory,
                Capability::Scheduler,
                Capability::Models,
            ]
        );
        assert_eq!(ATTACH_OOP_GRANTS, &[Capability::Events, Capability::Models]);
    }

    /// Before/after for each grant site. The expected names are the
    /// intersection those sites granted before the resolver existed, so
    /// editing a ceiling (or granting an extra capability) turns this red.
    #[test]
    fn the_three_sites_grant_the_same_set_as_before() {
        let documents = manifest(DOCUMENTS_YML);
        let documents_oop = resolved(&documents, GrantForm::OutOfProcess);
        assert_eq!(
            documents_oop.granted,
            legacy(&documents, GrantForm::OutOfProcess.ceiling())
        );
        assert_eq!(names(&documents_oop.granted), ["events", "models"]);
        assert_eq!(documents_oop.origin, ModuleOrigin::Unknown);
        assert_eq!(documents_oop.origin.to_string(), "unknown");

        let catalog: serde_json::Value = serde_json::from_str(DOCUMENTS_JSON).unwrap();
        let kernel: Vec<String> = catalog["requires"]["kernel"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        let operations: Vec<String> = catalog["requires"]["operations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        let from_catalog = RequiresSpec::parse(&kernel, &operations);
        assert!(from_catalog.is_valid());
        assert_eq!(from_catalog.kernel(), documents.kernel_capabilities());
        let catalog_oop = resolve(
            &from_catalog,
            GrantForm::OutOfProcess.host_cap(),
            Consent::FirstPartyEnabled,
        );
        assert_eq!(catalog_oop.granted, documents_oop.granted);

        let ear = agentear("events, models");
        let ear_grant = resolved(&ear, GrantForm::Attach);
        assert_eq!(ear_grant.granted, legacy(&ear, GrantForm::Attach.ceiling()));
        assert_eq!(names(&ear_grant.granted), ["events", "models"]);

        let ear_extra = agentear("events, models, memory, scheduler, approval");
        let ear_extra_grant = resolved(&ear_extra, GrantForm::Attach);
        assert_eq!(
            ear_extra_grant.granted,
            legacy(&ear_extra, GrantForm::Attach.ceiling())
        );
        assert_eq!(names(&ear_extra_grant.granted), ["events", "models"]);
        for item in ["memory", "scheduler", "approval"] {
            assert_eq!(reason(&ear_extra_grant, item), "above_ceiling", "{item}");
            assert!(
                !ear_extra_grant
                    .granted
                    .has(Capability::parse(item).unwrap())
            );
        }

        let os = in_process_os("events, memory, approval, scheduler");
        let os_grant = resolved(&os, GrantForm::InProcess);
        assert_eq!(
            os_grant.granted,
            legacy(&os, GrantForm::InProcess.ceiling())
        );
        assert_eq!(names(&os_grant.granted), ["events", "memory", "approval"]);
        assert_eq!(reason(&os_grant, "scheduler"), "above_ceiling");

        let node = manifest(NODE_MODULE_YML);
        let node_grant = resolved(&node, GrantForm::Attach);
        assert_eq!(
            node_grant.granted,
            legacy(&node, GrantForm::Attach.ceiling())
        );
        assert_eq!(names(&node_grant.granted), ["events"]);
    }

    /// A ceiling capability the module did not declare is not granted.
    /// Granting every ceiling entry (ignoring the declaration) turns this red.
    #[test]
    fn undeclared_host_capability_is_denied() {
        let only_models = in_process_os("models");
        let resolution = resolved(&only_models, GrantForm::InProcess);
        assert!(resolution.granted.iter().next().is_none());
        assert_eq!(
            resolution.granted,
            legacy(&only_models, GrantForm::InProcess.ceiling())
        );
        for item in ["events", "memory", "approval"] {
            assert_eq!(reason(&resolution, item), "not_declared", "{item}");
        }
        assert_eq!(reason(&resolution, "models"), "above_ceiling");
    }

    #[test]
    fn declared_above_the_ceiling_is_denied() {
        let documents = manifest(DOCUMENTS_YML);
        let in_process = resolved(&documents, GrantForm::InProcess);
        assert_eq!(names(&in_process.granted), ["events"]);
        assert_eq!(reason(&in_process, "models"), "above_ceiling");
        assert_eq!(
            in_process.granted,
            legacy(&documents, GrantForm::InProcess.ceiling())
        );
    }

    #[test]
    fn no_channel_denies_a_declared_ceiling_capability() {
        let spec = RequiresSpec::parse(&["events"], &[] as &[&str]);
        assert!(spec.is_valid());
        let resolution = resolve(
            &spec,
            HostCap {
                ceiling: &[Capability::Events],
                no_channel: &[Capability::Events],
            },
            Consent::FirstPartyEnabled,
        );
        assert!(resolution.granted.iter().next().is_none());
        assert_eq!(reason(&resolution, "events"), "no_channel");
    }

    #[test]
    fn invalid_requires_grants_nothing_and_names_the_token() {
        let cases: &[(&[&str], &[&str], &str)] = &[
            (&["model-gateway"], &[], "model-gateway"),
            (&["documents"], &[], "documents"),
            (&["telepathy"], &[], "telepathy"),
            (&["events"], &["models/read"], "models/read"),
            (&["events"], &["list"], "list"),
        ];
        for (kernel, operations, bad) in cases {
            let spec = RequiresSpec::parse(kernel, operations);
            assert!(!spec.is_valid(), "{bad}");
            assert!(spec.kernel().is_empty(), "{bad}");
            let resolution = resolve(
                &spec,
                GrantForm::OutOfProcess.host_cap(),
                Consent::FirstPartyEnabled,
            );
            assert!(
                resolution.granted.iter().next().is_none(),
                "{bad} must not grant events"
            );
            assert_eq!(reason(&resolution, bad), "requires_invalid");
            assert!(
                resolution
                    .denied
                    .iter()
                    .all(|d| d.reason == DenyReason::RequiresInvalid),
                "{bad}: {:?}",
                resolution.denied
            );
        }
    }

    #[test]
    fn a_canonical_operation_does_not_change_the_kernel_grant() {
        let spec = RequiresSpec::parse(&["events"], &["models.read"]);
        assert!(spec.is_valid());
        assert_eq!(spec.operations, vec![Operation::ModelsRead]);
        let resolution = resolve(
            &spec,
            GrantForm::InProcess.host_cap(),
            Consent::FirstPartyEnabled,
        );
        assert_eq!(names(&resolution.granted), ["events"]);
        assert!(resolution.denied.iter().all(|d| d.item != "models.read"));
    }

    #[test]
    fn shipped_locks_parse_and_hyphae_events_need_a_channel_to_be_withheld() {
        let idoris = agent24_components::ComponentLock::parse(IDORIS_LOCK).unwrap();
        let idoris_spec = RequiresSpec::parse(&idoris.requires.kernel, &idoris.requires.operations);
        assert!(idoris_spec.is_valid());
        assert!(idoris_spec.kernel().is_empty());
        let hyphae = agent24_components::ComponentLock::parse(HYPHAE_LOCK).unwrap();
        let hyphae_spec = RequiresSpec::parse(&hyphae.requires.kernel, &hyphae.requires.operations);
        assert_eq!(hyphae_spec.kernel(), &[Capability::Events]);
        let withheld = resolve(
            &hyphae_spec,
            HostCap {
                ceiling: &[Capability::Events],
                no_channel: &[Capability::Events],
            },
            Consent::FirstPartyEnabled,
        );
        assert!(withheld.granted.iter().next().is_none());
        assert_eq!(reason(&withheld, "events"), "no_channel");
    }

    #[test]
    fn the_three_grant_sites_go_through_the_resolver() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let domain = std::fs::read_to_string(root.join("src/domain.rs")).unwrap();
        let attach = std::fs::read_to_string(root.join("src/attach_grant.rs")).unwrap();
        let domain_prod = domain.split("pub(crate) mod tests").next().unwrap();
        let attach_prod = attach.split("mod tests").next().unwrap();
        assert_eq!(
            code_lines_containing(domain_prod, "requires::resolve("),
            2,
            "in-process and out-of-process grants must both call the resolver"
        );
        assert_eq!(
            code_lines_containing(attach_prod, "requires::resolve("),
            1,
            "attach grants must call the resolver"
        );
        assert_eq!(
            code_lines_containing(domain_prod, "Grants::granting"),
            0,
            "domain production code must not grant beside the resolver"
        );
        assert_eq!(
            code_lines_containing(attach_prod, "Grants::granting"),
            0,
            "attach production code must not grant beside the resolver"
        );
    }

    fn code_lines_containing(src: &str, needle: &str) -> usize {
        src.lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with("//") && trimmed.contains(needle)
            })
            .count()
    }
}
