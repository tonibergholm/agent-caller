//! Whether Git tracks a settings file: tracked, untracked or unknown.
use anyhow::{Result, bail};
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitStatus {
    Tracked,
    Untracked,
    Unknown(String),
}

fn git_program() -> String {
    #[cfg(debug_assertions)]
    if let Ok(p) = std::env::var("ACD_TEST_GIT") {
        return p;
    }
    "git".into()
}

/// Works for an absent file too: an indexed deleted file remains tracked.
pub fn status(path: &Path) -> GitStatus {
    let Some(name) = path.file_name() else {
        return GitStatus::Unknown("no file name".into());
    };
    let mut dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut rel = std::path::PathBuf::from(name);
    while !dir.is_dir() {
        let Some(parent) = dir.parent() else {
            return GitStatus::Untracked;
        };
        rel = std::path::PathBuf::from(dir.file_name().unwrap_or_default()).join(rel);
        dir = parent.to_path_buf();
    }
    match Command::new(git_program())
        .arg("-C")
        .arg(&dir)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(&rel)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
    {
        Err(e) => GitStatus::Unknown(format!("git could not run: {e}")),
        Ok(o) if o.status.success() => GitStatus::Tracked,
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            match o.status.code() {
                Some(1) => GitStatus::Untracked,
                Some(128) if err.contains("not a git repository") => GitStatus::Untracked,
                _ => GitStatus::Unknown(err.lines().next().unwrap_or("git failed").to_owned()),
            }
        }
    }
}

pub fn check(path: &Path, allow_tracked: bool, allow_unknown: bool) -> Result<()> {
    match status(path) {
        GitStatus::Untracked => Ok(()),
        GitStatus::Tracked if allow_tracked => Ok(()),
        GitStatus::Tracked => bail!(
            "{} is tracked by Git, and the hook token would be committed. Untrack it, or pass --allow-tracked",
            path.display()
        ),
        GitStatus::Unknown(_) if allow_unknown => Ok(()),
        GitStatus::Unknown(why) => bail!(
            "cannot tell whether {} is tracked by Git ({why}). Fix Git, or pass --allow-unknown-git",
            path.display()
        ),
    }
}
