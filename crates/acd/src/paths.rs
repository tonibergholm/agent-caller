use std::path::PathBuf;

/// `$ACD_CONFIG_DIR`, or `~/.config/acd`.
pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ACD_CONFIG_DIR") { return PathBuf::from(d); }
    let home = std::env::var_os("HOME").expect("HOME is set");
    PathBuf::from(home).join(".config").join("acd")
}
