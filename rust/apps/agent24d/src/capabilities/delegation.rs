//! R1 delegation records are absent on first start and are never migrated from enabled flags.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModuleSurface {
    pub name: String,
    pub version: String,
    pub namespace: String,
    pub routes_sha256: String,
}
#[derive(Default)]
pub struct Delegations {
    records: Mutex<BTreeMap<String, ModuleSurface>>,
    path: Option<PathBuf>,
}
impl Delegations {
    pub fn open(dir: &Path) -> io::Result<Self> {
        let path = dir.join("module-delegations.json");
        let records = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            records: Mutex::new(records),
            path: Some(path),
        })
    }
    pub fn permits(&self, surface: &ModuleSurface) -> bool {
        self.records
            .lock()
            .is_ok_and(|records| records.get(&surface.name) == Some(surface))
    }
    pub fn establish(&self, surface: &ModuleSurface) -> io::Result<()> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| io::Error::other("delegation lock poisoned"))?;
        let mut updated = records.clone();
        updated.insert(surface.name.clone(), surface.clone());
        if let Some(path) = &self.path {
            // Serialize writers under the same mutex; rename is atomic and no
            // in-memory authority changes until the durable replacement succeeds.
            let tmp =
                path.with_extension(format!("{}.writing", super::token::random_capability_id()));
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp)?;
            let result: io::Result<()> = (|| {
                file.write_all(&serde_json::to_vec(&updated)?)?;
                file.sync_all()?;
                std::fs::rename(&tmp, path)?;
                if let Some(parent) = path.parent() {
                    std::fs::File::open(parent)?.sync_all()?;
                }
                Ok(())
            })();
            if result.is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
            result?;
        }
        *records = updated;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[test]
    fn fresh_and_upgraded_state_require_delegation_and_changed_surface_invalidates_it() {
        let dir = tempfile::tempdir().unwrap();
        // Old enabled flags are deliberately not an authority source.
        std::fs::write(dir.path().join("os.json"), b"{\"enabled\":[\"probe\"]}").unwrap();
        let surface = ModuleSurface {
            name: "probe".into(),
            namespace: "/api/v1/probe".into(),
            version: "1".into(),
            routes_sha256: "routes-v1".into(),
        };
        let delegations = Delegations::open(dir.path()).unwrap();
        assert!(!delegations.permits(&surface));
        assert!(!Delegations::open(dir.path()).unwrap().permits(&surface));
        delegations.establish(&surface).unwrap();
        assert!(delegations.permits(&surface));
        let reopened = Delegations::open(dir.path()).unwrap();
        assert!(reopened.permits(&surface));
        let mut routes = surface.clone();
        routes.routes_sha256 = "new-route-set".into();
        assert!(!reopened.permits(&routes));
        let mut version = surface.clone();
        version.version = "2".into();
        assert!(!reopened.permits(&version));
    }
}
