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
fn the_pkgbuild_installs_exactly_the_embedded_plugin_files() {
    let pkgbuild = std::fs::read_to_string(root().join("packaging/arch/agent-caller-omarchy/PKGBUILD")).unwrap();
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
