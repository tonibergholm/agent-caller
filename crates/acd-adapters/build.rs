// Sets `cfg(owner_checks)` exactly as acd-securefs does, from the same rule
// file, so the client and the file checks can never disagree about a target.
include!("../acd-securefs/src/platform.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../acd-securefs/src/platform.rs");
    println!("cargo:rustc-check-cfg=cfg(owner_checks)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let unix = std::env::var_os("CARGO_CFG_UNIX").is_some();
    if platform_supports_owner_checks(&os, unix) {
        println!("cargo:rustc-cfg=owner_checks");
    }
}
