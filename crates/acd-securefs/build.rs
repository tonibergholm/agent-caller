// Sets `cfg(owner_checks)` on targets where the unix owner-only file checks
// are supported. The rule lives in src/platform.rs (see there for why it is not
// just `cfg(unix)`).
include!("src/platform.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/platform.rs");
    println!("cargo:rustc-check-cfg=cfg(owner_checks)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let unix = std::env::var_os("CARGO_CFG_UNIX").is_some();
    if platform_supports_owner_checks(&os, unix) {
        println!("cargo:rustc-cfg=owner_checks");
    }
}
