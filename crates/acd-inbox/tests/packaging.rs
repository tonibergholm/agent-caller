//! The Arch package for the Omarchy plugin installs the unit from
//! packaging/systemd and the plugin files listed in its PKGBUILD. Both must
//! stay what `acd-inbox install` uses, so the installer remains the source.
use std::path::{Path, PathBuf};

use acd_inbox::install::{PLUGIN_FILES, unit_text};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn the_packaged_unit_is_the_installer_unit_for_usr_bin() {
    let packaged = std::fs::read_to_string(root().join("packaging/systemd/acd-inbox.service")).unwrap();
    assert_eq!(packaged, unit_text(Path::new("/usr/bin/acd-inbox")));
}

#[test]
fn the_pkgbuilds_install_exactly_the_embedded_plugin_files() {
    for dir in ["agent-caller-omarchy", "agent-caller-omarchy-git"] {
        check_plugin_list(dir);
    }
}

fn check_plugin_list(dir: &str) {
    let pkgbuild = std::fs::read_to_string(root().join("packaging/arch").join(dir).join("PKGBUILD")).unwrap();
    let line = pkgbuild.lines().find(|l| l.starts_with("_plugin_files=(")).expect("_plugin_files array");
    let listed: Vec<&str> = line.trim_start_matches("_plugin_files=(").trim_end_matches(')').split_whitespace().collect();
    let mut want: Vec<&str> = PLUGIN_FILES.iter().map(|(n, _)| *n).collect();
    want.sort();
    let mut got = listed.clone();
    got.sort();
    assert_eq!(got, want);
    // And the files in the checkout are the ones the installer embeds.
    for (name, text) in PLUGIN_FILES {
        assert_eq!(std::fs::read_to_string(root().join("clients/omarchy").join(name)).unwrap(), *text);
    }
}

/// ~/.local/bin is first in PATH on Omarchy and holds the previous copy, so a
/// bare `acd-inbox install` would reinstall that copy's old plugin files.
#[test]
fn the_package_messages_run_the_packaged_binary() {
    let msg = std::fs::read_to_string(root().join("packaging/arch/agent-caller-omarchy/agent-caller-omarchy.install")).unwrap();
    assert!(msg.contains("/usr/bin/acd-inbox install"));
    // Commands are the indented lines; the prose names the command in backticks.
    for line in msg.lines().filter(|l| l.starts_with("    ") || l.contains("Check it with:")) {
        assert!(!line.trim_start().starts_with("acd-inbox "), "bare command in: {line}");
        assert!(!line.contains("with: acd-inbox"), "bare command in: {line}");
    }
    let docs = std::fs::read_to_string(root().join("docs/packaging.md")).unwrap();
    for line in docs.lines() {
        assert!(!line.starts_with("acd-inbox install"), "bare command in docs: {line}");
    }
}

/// The smoke test must fail for a stopped service although `acd-inbox status`
/// exits 0; its selftest runs the real script against stubs.
#[cfg(target_os = "linux")]
#[test]
fn the_smoke_test_rejects_a_stopped_service() {
    let out = std::process::Command::new("bash").arg(root().join("packaging/smoke-test-selftest.sh")).output().unwrap();
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}
