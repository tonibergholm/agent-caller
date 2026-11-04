// Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
// Alone in its file because it changes process environment variables.
#![cfg(owner_checks)]

use std::path::PathBuf;

/// local.rs:11 / 16 `default_config_dir`: `ACD_CONFIG_DIR` wins, else `$HOME/.config/acd`, else none.
#[test]
fn the_default_config_dir_follows_the_environment() {
    // SAFETY: this is the only test in this binary, so nothing else reads the environment.
    unsafe {
        std::env::set_var("ACD_CONFIG_DIR", "/somewhere/acd");
        std::env::set_var("HOME", "/home/someone");
    }
    assert_eq!(acd_client::local::default_config_dir(), Some(PathBuf::from("/somewhere/acd")));
    unsafe { std::env::remove_var("ACD_CONFIG_DIR") };
    assert_eq!(acd_client::local::default_config_dir(), Some(PathBuf::from("/home/someone/.config/acd")));
    unsafe { std::env::remove_var("HOME") };
    assert_eq!(acd_client::local::default_config_dir(), None);
}
