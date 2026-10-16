// sqlx::migrate! embeds migrations at compile time; rebuild when one is added
// or edited even if no Rust file changed.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
