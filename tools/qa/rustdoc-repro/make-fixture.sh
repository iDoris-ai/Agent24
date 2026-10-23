#!/usr/bin/env bash
# ---- fail-closed（w38）：历史研究脚本，未通过安全验收（见 docs/agent/cloud-wave38）。----
# 已知缺陷：无校验 rm -rf / 复用已有目录 / 删除预存 stop / 跟随 OUT 符号链接覆写。
# 没有任何环境变量或参数可以解除此拒绝；只能作为阅读材料保留。
echo "refused: 历史脚本 $(basename "$0") 已停用（未安全验收），不执行任何操作" >&2
exit 3
# ---- 以下为历史内容，保持原样，永不执行 ----
# 生成最小 Rust fixture：一个共享 proc-macro（模拟 registry 的 async-trait：两个 workspace 引用同一 PackageId），
# 两个 source 路径不同、内容相同的 workspace（ws_a / ws_b），结构模拟 agent24-agent -> agent24-store -> async-trait。
# 用法: make-fixture.sh <ROOT>   （ROOT 必须是空目录或不存在，建议在 /tmp 或 scratchpad 下）
set -euo pipefail
ROOT=${1:?usage: make-fixture.sh <ROOT>}
mkdir -p "$ROOT"
ROOT=$(cd "$ROOT" && pwd)

# ---- 共享 proc-macro（等价于 registry 依赖：两个 workspace 看到同一 PackageId -> 同一 -C metadata 哈希）----
mkdir -p "$ROOT/shared/fake_async_trait/src"
cat > "$ROOT/shared/fake_async_trait/Cargo.toml" <<'EOF'
[package]
name = "fake_async_trait"
version = "0.1.0"
edition = "2021"
[lib]
proc-macro = true
EOF
cat > "$ROOT/shared/fake_async_trait/src/lib.rs" <<'EOF'
extern crate proc_macro;
use proc_macro::TokenStream;
/// 透传属性宏（只为让下游必须在 rustdoc --test 期间 dlopen 本 dylib）。
#[proc_macro_attribute]
pub fn fake_async_trait(_attr: TokenStream, item: TokenStream) -> TokenStream { item }
// REBUILD_SALT
EOF

for ws in ws_a ws_b; do
  W="$ROOT/$ws"
  mkdir -p "$W/core/src" "$W/store/src" "$W/agent/src" "$W/agent/tests"
  cat > "$W/Cargo.toml" <<'EOF'
[workspace]
resolver = "2"
members = ["core", "store", "agent"]
EOF
  # fx_core：只被 fx_store 依赖；对 fx_agent 的 doctest 是传递依赖（rustdoc 经 -L dependency= 按 名字+SVH 搜索，而非 --extern）
  printf '[package]\nname = "fx_core"\nversion = "0.1.0"\nedition = "2021"\n' > "$W/core/Cargo.toml"
  echo 'pub fn base() -> u32 { 7 }' > "$W/core/src/lib.rs"
  cat > "$W/store/Cargo.toml" <<EOF
[package]
name = "fx_store"
version = "0.1.0"
edition = "2021"
[dependencies]
fake_async_trait = { path = "$ROOT/shared/fake_async_trait" }
fx_core = { path = "../core" }
EOF
  cat > "$W/store/src/lib.rs" <<'EOF'
#[fake_async_trait::fake_async_trait]
pub trait Store { fn get(&self) -> u32; }
pub struct Mem;
impl Store for Mem { fn get(&self) -> u32 { fx_core::base() } }
#[cfg(test)]
mod tests { #[test] fn unit() { use super::Store; assert_eq!(super::Mem.get(), 7); } }
EOF
  cat > "$W/agent/Cargo.toml" <<EOF
[package]
name = "fx_agent"
version = "0.1.0"
edition = "2021"
[dependencies]
fake_async_trait = { path = "$ROOT/shared/fake_async_trait" }
fx_store = { path = "../store" }
EOF
  cat > "$W/agent/src/lib.rs" <<'EOF'
//! 代理 crate。
//!
//! ```
//! assert_eq!(fx_agent::run(), 7);
//! ```
use fx_store::Store;
#[fake_async_trait::fake_async_trait]
pub trait Runner { fn go(&self) -> u32; }
pub fn run() -> u32 { fx_store::Mem.get() }
#[cfg(test)]
mod tests { #[test] fn unit() { assert_eq!(super::run(), 7); } }
EOF
  # 集成测试：FX_SLEEP 秒，用来模拟"3154 个测试跑很久之后才轮到 doctest"的时间窗；FX_HOOK 文件出现则表示进入窗口
  cat > "$W/agent/tests/slow.rs" <<'EOF'
#[test]
fn slow_window() {
    if let Ok(p) = std::env::var("FX_HOOK") { let _ = std::fs::write(p, b"in-test"); }
    let s: u64 = std::env::var("FX_SLEEP").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    std::thread::sleep(std::time::Duration::from_secs(s));
    assert_eq!(fx_agent::run(), 7);
}
EOF
done
echo "fixture at $ROOT"
