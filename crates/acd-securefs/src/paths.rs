//! Where acd keeps its files on Windows, in one place. Pure string logic over
//! environment values, so the rules are tested on any host; `windows_*_dir`
//! reads the real environment and exists only on Windows.
//!
//! The rules:
//! - Everything lives under `%LOCALAPPDATA%\acd`, never `%APPDATA%`. `%APPDATA%`
//!   (Roaming) is copied to other machines by roaming profiles and folder
//!   redirection; a token or a pinned-certificate list must not travel.
//! - `config` holds what the daemon and clients read at startup (tokens, TLS
//!   key, settings); `data` holds state they may rebuild (journals, logs).
//!   Both are private directories (see `winacl`), and the split lets a backup
//!   tool skip `data`.
//! - `ACD_CONFIG_DIR` overrides the config dir, as on Unix, but only with an
//!   absolute path with a drive letter or a UNC share. A relative path would
//!   resolve against the working directory of whatever started the process.
//! - If `LOCALAPPDATA` is missing or not absolute there is no directory:
//!   there is no fallback to the current directory, `%TEMP%` or `%APPDATA%`.
#![cfg_attr(not(windows), allow(dead_code))]

/// The directory under `%LOCALAPPDATA%` that holds everything.
pub const APP_DIR: &str = "acd";
pub const CONFIG_SUBDIR: &str = "config";
pub const DATA_SUBDIR: &str = "data";

/// `C:\...` or `\\server\share\...` (also `\\?\C:\...`). A drive-relative
/// `C:foo`, a rooted-but-driveless `\foo` and a relative `foo` are not.
pub fn is_absolute_windows(p: &str) -> bool {
    let b = p.as_bytes();
    if p.contains('\0') || p.len() < 3 {
        return false;
    }
    let drive = b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/');
    let unc = (p.starts_with("\\\\") || p.starts_with("//"))
        && p.len() > 2
        && !matches!(b[2], b'\\' | b'/');
    drive || unc
}

fn join(base: &str, parts: &[&str]) -> String {
    let mut out = base.trim_end_matches(['\\', '/']).replace('/', "\\");
    for p in parts {
        out.push('\\');
        out.push_str(p);
    }
    out
}

/// A `..` component would let an environment value escape the directory it
/// names; the override is refused if it has one.
fn has_dotdot(p: &str) -> bool {
    p.split(['\\', '/']).any(|c| c == "..")
}

fn local_base(local_appdata: Option<&str>) -> Option<&str> {
    local_appdata.filter(|l| is_absolute_windows(l) && !has_dotdot(l))
}

/// The config dir: `override_dir` if it is an acceptable absolute path,
/// otherwise `<LOCALAPPDATA>\acd\config`. A present but unacceptable override
/// is an error (`None`), not a fall-through to the default: the user asked for
/// somewhere specific and must not silently get somewhere else.
pub fn config_dir(local_appdata: Option<&str>, override_dir: Option<&str>) -> Option<String> {
    match override_dir {
        Some(o) if is_absolute_windows(o) && !has_dotdot(o) => Some(o.replace('/', "\\")),
        Some(_) => None,
        None => local_base(local_appdata).map(|l| join(l, &[APP_DIR, CONFIG_SUBDIR])),
    }
}

/// The data dir: `<LOCALAPPDATA>\acd\data`. It does not follow the config
/// override, so a config dir moved to a share never moves the rebuildable state.
pub fn data_dir(local_appdata: Option<&str>) -> Option<String> {
    local_base(local_appdata).map(|l| join(l, &[APP_DIR, DATA_SUBDIR]))
}

/// True if `path` is below `%APPDATA%` (Roaming). Used by the app shell to
/// refuse a Tauri `app_config_dir()` (which is Roaming on Windows) as a place
/// for settings or secrets.
pub fn is_under_roaming(path: &str, appdata: Option<&str>) -> bool {
    let Some(a) = appdata.filter(|a| is_absolute_windows(a)) else {
        return false;
    };
    let norm = |s: &str| s.replace('/', "\\").trim_end_matches('\\').to_ascii_lowercase();
    let (p, a) = (norm(path), norm(a));
    p == a || p.starts_with(&format!("{a}\\"))
}

#[cfg(windows)]
pub fn windows_config_dir() -> Option<std::path::PathBuf> {
    let local = std::env::var("LOCALAPPDATA").ok();
    // A value that is not Unicode counts as an override that is unacceptable.
    let over = match std::env::var("ACD_CONFIG_DIR") {
        Ok(v) => Some(v),
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => return None,
    };
    config_dir(local.as_deref(), over.as_deref()).map(Into::into)
}

#[cfg(windows)]
pub fn windows_data_dir() -> Option<std::path::PathBuf> {
    data_dir(std::env::var("LOCALAPPDATA").ok().as_deref()).map(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: &str = r"C:\Users\ann\AppData\Local";
    const ROAM: &str = r"C:\Users\ann\AppData\Roaming";

    #[test]
    fn absolute_paths() {
        for ok in [r"C:\x", "c:/x", r"D:\a\b", r"\\srv\share", r"\\srv\share\x", r"\\?\C:\x", "//srv/share"] {
            assert!(is_absolute_windows(ok), "{ok}");
        }
        for bad in ["", "x", r"x\y", r"\x", r"C:", r"C:x", "1:\\x", r"\\", r"\\\x", "/x", "C:\\x\0y", "~/x", "$HOME/x"] {
            assert!(!is_absolute_windows(bad), "{bad:?}");
        }
    }

    #[test]
    fn default_dirs_are_under_localappdata_not_appdata() {
        assert_eq!(config_dir(Some(LOCAL), None).unwrap(), r"C:\Users\ann\AppData\Local\acd\config");
        assert_eq!(data_dir(Some(LOCAL)).unwrap(), r"C:\Users\ann\AppData\Local\acd\data");
        for d in [config_dir(Some(LOCAL), None).unwrap(), data_dir(Some(LOCAL)).unwrap()] {
            assert!(!is_under_roaming(&d, Some(ROAM)), "{d}");
        }
    }

    #[test]
    fn a_trailing_separator_does_not_double_up() {
        assert_eq!(config_dir(Some(r"C:\L\"), None).unwrap(), r"C:\L\acd\config");
        assert_eq!(data_dir(Some("C:/L/")).unwrap(), r"C:\L\acd\data");
    }

    #[test]
    fn no_localappdata_means_no_directory_and_no_fallback() {
        assert_eq!(config_dir(None, None), None);
        assert_eq!(data_dir(None), None);
        for bad in ["", "relative", r"\rooted", "C:drive-relative", r"..\x", r"C:\a\..\..\b"] {
            assert_eq!(config_dir(Some(bad), None), None, "{bad:?}");
            assert_eq!(data_dir(Some(bad)), None, "{bad:?}");
        }
    }

    #[test]
    fn the_override_wins_when_absolute_and_never_falls_through() {
        assert_eq!(config_dir(Some(LOCAL), Some(r"D:\acd-test")).unwrap(), r"D:\acd-test");
        assert_eq!(config_dir(None, Some("D:/acd/test")).unwrap(), r"D:\acd\test");
        // It works without LOCALAPPDATA too.
        assert_eq!(config_dir(None, Some(r"D:\acd-test")).unwrap(), r"D:\acd-test");
        for bad in ["", "rel", r"\x", r"D:\a\..\b", "D:rel"] {
            assert_eq!(config_dir(Some(LOCAL), Some(bad)), None, "{bad:?}");
        }
    }

    #[test]
    fn data_dir_ignores_the_config_override() {
        assert_eq!(data_dir(Some(LOCAL)).unwrap(), r"C:\Users\ann\AppData\Local\acd\data");
    }

    #[test]
    fn roaming_detection() {
        assert!(is_under_roaming(r"C:\Users\ann\AppData\Roaming\dev.agentcaller", Some(ROAM)));
        assert!(is_under_roaming(r"c:/users/ANN/appdata/roaming", Some(ROAM)));
        assert!(is_under_roaming(&format!("{ROAM}\\"), Some(ROAM)));
        // A sibling that merely shares the prefix is not under it.
        assert!(!is_under_roaming(r"C:\Users\ann\AppData\RoamingX\a", Some(ROAM)));
        assert!(!is_under_roaming(r"C:\Users\ann\AppData\Local\acd", Some(ROAM)));
        // With no usable APPDATA nothing can be judged roaming.
        assert!(!is_under_roaming(ROAM, None));
        assert!(!is_under_roaming(ROAM, Some("rel")));
    }
}
