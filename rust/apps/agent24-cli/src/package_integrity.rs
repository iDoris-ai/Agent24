//! `agent24 package-integrity` — local generate/verify. No daemon.

use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum Action {
    /// 扫描发布目录，写 files.json，并把 integrity 段写入 domain-os.yml。
    /// 不签名。可执行文件要在调用前签完。
    Generate {
        /// 发布 staging 目录，里面已有 domain-os.yml
        #[arg(long)]
        root: PathBuf,
        /// 包内相对路径。可重复。必须包含 spawn.command。
        #[arg(long, required = true)]
        executable: Vec<String>,
    },
    /// 只读、严格校验。成功不输出；失败输出固定原因和相对路径。
    Verify {
        #[arg(long)]
        root: PathBuf,
    },
}

pub fn run(action: Action) -> Result<(), String> {
    match action {
        Action::Generate { root, executable } => {
            agent24_os_packages::package_integrity::generate(&root, &executable)
                .map_err(|err| err.to_string())
        }
        Action::Verify { root } => {
            agent24_os_packages::package_integrity::verify(&root).map_err(|err| err.to_string())
        }
    }
}
