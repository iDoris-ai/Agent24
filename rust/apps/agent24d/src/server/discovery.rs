use tokio_util::sync::CancellationToken;

/// Append packages found on disk to a build-time catalogue.
///
/// Everything the caller passes in is a build-time entry. Everything this adds
/// was found on disk AFTER the binary was built, which is the only shape that can
/// demonstrate "installing a third-party domain OS needs no kernel change" — a
/// mock appended to the `vec!` in `serve` would prove nothing, because reaching
/// that `vec!` means editing and rebuilding the daemon.
///
/// A discovered package is NOT constructed here, and cannot be: an out-of-process
/// module has no Rust type. It becomes a [`crate::domain::Build::Package`], which
/// the mounter starts under a supervisor — only if it is admissible and enabled —
/// and which appears in `agent24 os list` either way.
///
/// **Order is load-bearing.** Discovered entries go AFTER the built-in ones, and
/// `mount_all` claims names first-come-first-served, so a disk package cannot
/// shadow a compiled-in module by taking its name. The second claimant gets an
/// explicit refusal rather than silently winning.
/// A panicked scan is not an empty catalogue. An empty catalogue means no
/// packages were installed; a join failure means startup did not learn that.
pub(super) fn catalogue_from_join<T>(
    joined: Result<T, tokio::task::JoinError>,
) -> Result<T, std::io::Error> {
    joined.map_err(|err| {
        tracing::error!("discovering domain OS packages failed: {err}");
        std::io::Error::other(format!("discovering domain OS packages failed: {err}"))
    })
}

/// Register package tools unless shutdown has already cancelled the token.
/// A package the scan accepted is not published after that.
pub(crate) fn register_discovered_module_tools(
    catalogue: &[crate::domain::Installed],
    cancel: &CancellationToken,
    module_registry: &mut agent24_domain::tool::ModuleToolRegistry,
    module_operations: &mut std::collections::HashSet<(String, String)>,
) {
    if cancel.is_cancelled() {
        tracing::info!("not registering domain OS tools: shutdown already cancelled");
        return;
    }
    for entry in catalogue {
        if let crate::domain::Build::Package(package) = &entry.build {
            if let Err(error) =
                module_registry.register_module(entry.name.as_str(), package.manifest.tools())
            {
                tracing::warn!(module = entry.name, %error, "module tools were refused");
                continue;
            }
            module_operations.extend(
                package
                    .manifest
                    .tools()
                    .iter()
                    .map(|tool| (entry.name.clone(), tool.operation().to_owned())),
            );
        }
    }
}

/// Drop tools filled before the publish point if the token has since fired.
pub(crate) fn module_tools_if_live(
    cancel: &CancellationToken,
    registry: agent24_domain::tool::ModuleToolRegistry,
    operations: std::collections::HashSet<(String, String)>,
) -> (
    agent24_domain::tool::ModuleToolRegistry,
    std::collections::HashSet<(String, String)>,
) {
    if cancel.is_cancelled() {
        tracing::info!("not publishing domain OS tools: shutdown already cancelled");
        (
            agent24_domain::tool::ModuleToolRegistry::new(),
            std::collections::HashSet::new(),
        )
    } else {
        (registry, operations)
    }
}

pub(super) fn os_signature_policy() -> agent24_os_packages::signature::Policy {
    #[cfg(test)]
    {
        agent24_os_packages::test_support::signature_policy()
    }
    #[cfg(not(test))]
    {
        agent24_os_packages::signature::Policy::default()
    }
}

pub(super) struct DiscoveryPass {
    pub(super) catalogue: Vec<crate::domain::Installed>,
    /// Integrity refusals whose directory is a package, not the packages root.
    /// The root's own unreadability stays a log line: it is not a component.
    pub(super) integrity_refusals: Vec<agent24_os_packages::discovery::Refused>,
}

/// Catalogue directories plus the directory of each integrity refusal, under
/// the component name [`crate::domain::integrity_refusal_reports`] assigned.
/// Enable rescans that directory. A mounted name keeps the catalogue path.
pub(super) fn package_dirs_for_startup(
    catalogue: &[crate::domain::Installed],
    integrity_refusals: &[agent24_os_packages::discovery::Refused],
    mounted: &[crate::domain::MountReport],
) -> (
    Vec<crate::domain::MountReport>,
    std::collections::HashMap<String, std::path::PathBuf>,
) {
    let rows = crate::domain::integrity_refusal_reports(integrity_refusals, mounted);
    let mut dirs = std::collections::HashMap::new();
    for entry in catalogue {
        if let crate::domain::Build::Package(package) = &entry.build {
            dirs.insert(entry.name.clone(), package.dir.clone());
        }
    }
    for (row, refused) in rows
        .iter()
        .zip(integrity_refusals.iter().filter(|r| r.integrity.is_some()))
    {
        dirs.entry(row.name.clone())
            .or_insert_with(|| refused.dir.clone());
    }
    (rows, dirs)
}

#[cfg(test)]
pub(super) fn with_discovered(
    catalogue: Vec<crate::domain::Installed>,
    packages_root: &std::path::Path,
    cancel: &CancellationToken,
) -> DiscoveryPass {
    with_discovered_policy(catalogue, packages_root, cancel, &os_signature_policy())
}
pub(super) fn with_discovered_policy(
    mut catalogue: Vec<crate::domain::Installed>,
    packages_root: &std::path::Path,
    cancel: &CancellationToken,
    signature: &agent24_os_packages::signature::Policy,
) -> DiscoveryPass {
    // FU-41. Checked HERE, at the one place packages are read, rather than at
    // startup: a check somewhere else can be true when it runs and false when
    // the directory is used, and this is the moment of use.
    //
    // A refusal SKIPS discovery entirely and keeps the compiled-in catalogue.
    // That is the safe direction — a daemon with only its built-in modules still
    // works — and it is loud, because from ME-3b-3 a package in a directory
    // somebody else can write decides what this process executes.
    if let Err(e) = agent24_os_packages::ensure_packages_root(packages_root) {
        tracing::error!("{e}; no disk packages will be loaded this run");
        return DiscoveryPass {
            catalogue,
            integrity_refusals: Vec::new(),
        };
    }
    let scan = agent24_os_packages::discovery::scan_with_policy(packages_root, cancel, signature);
    let mut integrity_refusals = Vec::new();
    for dir in &scan.cancelled {
        tracing::info!(
            "domain OS package at {} was not loaded: verification cancelled",
            dir.display()
        );
    }
    for r in &scan.refused {
        tracing::warn!(
            "domain OS package at {} was not loaded: {}",
            r.dir.display(),
            r.why
        );
        if r.integrity.is_some() && r.dir.as_path() != packages_root {
            integrity_refusals.push(r.clone());
        }
    }
    for d in scan.found {
        let name = d.manifest.name().to_owned();
        let version = d.manifest.version().to_owned();
        tracing::info!(
            "discovered domain OS {name:?} v{version} at {}",
            d.dir.display()
        );
        catalogue.push(crate::domain::Installed {
            name,
            version,
            build: crate::domain::Build::Package(Box::new(crate::domain::Package {
                manifest: d.manifest,
                dir: d.dir,
                digest: d.digest,
            })),
        });
    }
    DiscoveryPass {
        catalogue,
        integrity_refusals,
    }
}
