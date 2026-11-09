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
        /// 目标包的 CPU 架构；verify/安装时仍按本机架构检查。
        #[arg(long, default_value = std::env::consts::ARCH, value_parser = ["aarch64", "x86_64"])]
        arch: String,
    },
    /// 只读、严格校验。成功不输出；失败输出固定原因和相对路径。
    Verify {
        #[arg(long)]
        root: PathBuf,
        /// 包的目标 CPU 架构（发布脚本自检跨架构包时用）；默认本机。
        /// daemon 扫描与安装始终按本机架构检查，不经这里。
        #[arg(long, default_value = std::env::consts::ARCH, value_parser = ["aarch64", "x86_64"])]
        arch: String,
    },
}

pub fn run(action: Action) -> Result<(), String> {
    match action {
        Action::Generate {
            root,
            executable,
            arch,
        } => agent24_os_packages::package_integrity::generate_for_arch(&root, &executable, &arch)
            .map_err(|err| err.to_string()),
        Action::Verify { root, arch } => {
            agent24_os_packages::package_integrity::verify_for_arch(&root, &arch)
                .map_err(|err| err.to_string())
        }
    }
}
