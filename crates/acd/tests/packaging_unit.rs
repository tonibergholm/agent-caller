//! packaging/systemd/acd.service is the unit the Arch package installs. It
//! must stay what `acd service install` writes, with only the executable
//! (`/usr/bin/acd`) and the home directory (`%h`) fixed by the package.
#[cfg(target_os = "linux")]
mod common;

#[cfg(target_os = "linux")]
use common::{acd_home, init_dir};

#[cfg(target_os = "linux")]
#[test]
fn the_packaged_unit_matches_what_service_install_writes() {
    let home = tempfile::tempdir().unwrap();
    let dir = init_dir(home.path());
    let exe = env!("CARGO_BIN_EXE_acd");
    let out = acd_home(&dir, home.path())
        .args(["service", "install", "--print"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let printed = String::from_utf8(out.stdout).unwrap();
    let config = std::fs::canonicalize(&dir).unwrap();
    // The package cannot know the user's home, so systemd's %h stands in. The
    // default config directory is ~/.config/acd, which is what %h/.config/acd names.
    let normalized = printed
        .replace(&config.display().to_string(), "%h/.config/acd")
        .replace(exe, "/usr/bin/acd");
    let packaged = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packaging/systemd/acd.service")).unwrap();
    // `print` ends with the println! newline.
    assert_eq!(packaged.trim_end_matches('\n'), normalized.trim_end_matches('\n'));
}
