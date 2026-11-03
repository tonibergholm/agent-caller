// The one rule for "this target can do owner-only file checks".
//
// `cfg(unix)` is not enough: Android has it set, but acd's owner/mode checks
// have not been proved there, and an app sandbox is no reason to trust them.
// So only the targets below take the real path; every other target (Windows,
// Android, anything new) refuses. A new unix target is refused until it is
// added here on purpose.
//
// This file is `include!`d by the build.rs of `acd-securefs` and `acd-client`,
// which turn it into `cfg(owner_checks)`. It is also compiled into
// `acd_securefs::platform_supports_owner_checks` so a test on any host can
// assert the decision for any target. It must stay free of `use` items.

// `target_os` is Cargo's `CARGO_CFG_TARGET_OS`, `unix` is `CARGO_CFG_UNIX`.
pub fn platform_supports_owner_checks(target_os: &str, unix: bool) -> bool {
    unix && target_os != "android"
}
