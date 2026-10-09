//! `acd hooks install|uninstall claude|codex` settings primitives.
#![allow(dead_code)]

mod entries;
mod json;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub const MARKER: &str = "acd-managed:v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Target {
    Claude,
    Codex,
}

pub struct Ctx {
    pub home: PathBuf,
    pub claude_dir: PathBuf,
    pub codex_dir: PathBuf,
    pub config_dir: PathBuf,
    pub exe: PathBuf,
    pub hook_port: u16,
    pub permission_hold: u64,
}

impl Ctx {
    pub fn from_env(config_dir: &Path) -> Result<Ctx> {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        let cfg = crate::config::Config::load(config_dir)?;
        let env_dir = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Ok(Ctx {
            claude_dir: env_dir("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude")),
            codex_dir: env_dir("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
            home,
            config_dir: std::fs::canonicalize(config_dir)?,
            exe: std::fs::canonicalize(std::env::current_exe()?)?,
            hook_port: cfg.hook_port,
            permission_hold: cfg.claude.permission_hold,
        })
    }

    pub fn settings_path(&self, target: Target) -> PathBuf {
        match target {
            Target::Claude => self.claude_dir.join("settings.json"),
            Target::Codex => self.codex_dir.join("hooks.json"),
        }
    }
}
