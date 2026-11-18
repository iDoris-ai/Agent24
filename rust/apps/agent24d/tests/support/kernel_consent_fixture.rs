//! Black-box packages are not on the bundled list, so the kernel lends them
//! nothing until a consent row exists. These tests are about the handle, so
//! they record that row before the daemon that mounts the package starts.

use std::path::Path;

pub fn allow_kernel(home: &Path, module: &str, caps: &[&str]) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("fixture runtime");
    rt.block_on(async {
        let dir = home.join(".agent24");
        std::fs::create_dir_all(&dir).expect("data dir");
        let store = agent24_store::Store::open(&dir.join("agent24.db"))
            .await
            .expect("open agent24.db");
        let mut names: Vec<String> = caps.iter().map(|cap| (*cap).to_owned()).collect();
        names.sort();
        names.dedup();
        let now = chrono::Utc::now();
        store
            .upsert_kernel_consent(&agent24_store::KernelConsentRow {
                module: module.to_owned(),
                fingerprint: format!("k={};o=", names.join(",")),
                capabilities: names,
                team_id: None,
                version: "0.1.0".into(),
                decided_at: now.to_rfc3339(),
                expires_at: (now + chrono::Duration::days(30)).to_rfc3339(),
            })
            .await
            .expect("record kernel consent");
        store.close().await.expect("close agent24.db");
    });
}
