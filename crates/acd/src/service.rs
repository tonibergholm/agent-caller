//! `acd service install`: launchd on macOS and a systemd user unit on Linux.
//! Both restart acd after any non-zero exit and leave clean stops alone.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub const LABEL: &str = "dev.agent-caller.acd";
pub const MARKER: &str = "acd-managed:v1";
const PATH_MAC: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
const PATH_LINUX: &str = "/usr/local/bin:/usr/bin:/bin";

pub trait Runner {
    fn run(&self, program: &str, args: &[String]) -> Result<()>;
}

pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<()> {
        let status = std::process::Command::new(program)
            .args(args)
            .status()
            .with_context(|| format!("running {program} {}", args.join(" ")))?;
        if !status.success() {
            bail!("{program} {} failed ({status})", args.join(" "));
        }
        Ok(())
    }
}

#[cfg(debug_assertions)]
struct FileRunner(PathBuf, Option<String>);

#[cfg(debug_assertions)]
impl Runner for FileRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<()> {
        use std::io::Write;

        let line = format!("{program} {}", args.join(" "));
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.0)
            .with_context(|| format!("opening service runner log {}", self.0.display()))?;
        writeln!(file, "{line}").context("recording service runner call")?;
        if self.1.as_ref().is_some_and(|word| line.contains(word)) {
            bail!("{line} failed (injected)");
        }
        Ok(())
    }
}

pub fn runner() -> Box<dyn Runner> {
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("ACD_TEST_SERVICE_RUNNER") {
        return Box::new(FileRunner(
            PathBuf::from(path),
            std::env::var("ACD_TEST_SERVICE_FAIL").ok(),
        ));
    }
    Box::new(SystemRunner)
}

fn write_definition(path: &Path, text: &str, force: bool) -> Result<bool> {
    std::fs::create_dir_all(path.parent().expect("service path has parent"))?;
    let had_definition = match std::fs::read_to_string(path) {
        Ok(old) => {
            if !old.contains(MARKER) && !force {
                bail!(
                    "{} exists and was not written by acd; rerun with --force to replace it (a backup is kept)",
                    path.display()
                );
            }
            let name = path
                .file_name()
                .expect("service path has a name")
                .to_string_lossy();
            let backup = path.with_file_name(format!(
                "{name}.bak-{}",
                chrono::Utc::now().timestamp_millis()
            ));
            std::fs::copy(path, &backup).with_context(|| {
                format!("backing up {} to {}", path.display(), backup.display())
            })?;
            if old == text {
                return Ok(true);
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    crate::secrets::write_atomic(path, text.as_bytes(), 0o644)?;
    Ok(had_definition)
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn launchd_plist(exe: &Path, config_dir: &Path, log: &Path) -> String {
    let exe = xml(&exe.to_string_lossy());
    let dir = xml(&config_dir.to_string_lossy());
    let log = xml(&log.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<!-- {MARKER} -->
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>run</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>ACD_CONFIG_DIR</key>
		<string>{dir}</string>
		<key>PATH</key>
		<string>{PATH_MAC}</string>
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ThrottleInterval</key>
	<integer>10</integer>
	<key>StandardErrorPath</key>
	<string>{log}</string>
	<key>StandardOutPath</key>
	<string>{log}</string>
</dict>
</plist>
"#
    )
}

fn unit_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn systemd_unit(exe: &Path, config_dir: &Path) -> String {
    format!(
        "# {MARKER}\n[Unit]\nDescription=agent-caller daemon (acd)\nAfter=network-online.target\n\n[Service]\nExecStart={} run\nEnvironment={}\nEnvironment=\"PATH={PATH_LINUX}\"\nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n",
        unit_quote(&exe.to_string_lossy()),
        unit_quote(&format!("ACD_CONFIG_DIR={}", config_dir.display())),
    )
}

pub fn install(
    home: &Path,
    exe: &Path,
    config_dir: &Path,
    print: bool,
    force: bool,
    runner: &dyn Runner,
) -> Result<String> {
    if cfg!(target_os = "macos") {
        let plist = home
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"));
        let text = launchd_plist(exe, config_dir, &home.join("Library/Logs/acd.log"));
        if print {
            return Ok(text);
        }
        std::fs::create_dir_all(home.join("Library/Logs"))?;
        let existed = write_definition(&plist, &text, force)?;
        let uid = acd_securefs::euid();
        if existed {
            runner
                .run(
                    "launchctl",
                    &["bootout".into(), format!("gui/{uid}/{LABEL}")],
                )
                .context("launchctl bootout failed")?;
        }
        runner
            .run(
                "launchctl",
                &[
                    "bootstrap".into(),
                    format!("gui/{uid}"),
                    plist.display().to_string(),
                ],
            )
            .context("launchctl bootstrap failed")?;
        Ok(format!(
            "Installed and started {}. Stop it with: launchctl bootout gui/{uid}/{LABEL}",
            plist.display()
        ))
    } else if cfg!(target_os = "linux") {
        let unit = home.join(".config/systemd/user/acd.service");
        let text = systemd_unit(exe, config_dir);
        if print {
            return Ok(text);
        }
        write_definition(&unit, &text, force)?;
        for args in [
            &["daemon-reload"][..],
            &["enable", "acd.service"],
            &["restart", "acd.service"],
        ] {
            let mut call = vec!["--user".to_string()];
            call.extend(args.iter().map(|arg| (*arg).to_string()));
            let call_name = args.join(" ");
            runner
                .run("systemctl", &call)
                .with_context(|| format!("systemctl {call_name} failed"))?;
        }
        Ok(format!(
            "Installed and started {}. Stop it with: systemctl --user disable --now acd.service",
            unit.display()
        ))
    } else {
        bail!("acd service install supports macOS and Linux")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;
    use std::cell::RefCell;

    #[test]
    fn the_plist_restarts_on_failure_only_and_sets_a_usable_path() {
        let plist = launchd_plist(
            Path::new("/opt/acd & co/acd"),
            Path::new("/Users/u/.config/acd"),
            Path::new("/Users/u/Library/Logs/acd.log"),
        );
        assert!(plist.contains("<string>/opt/acd &amp; co/acd</string>"));
        assert!(plist.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
        assert!(plist.contains(PATH_MAC));
        assert!(plist.contains("<key>ACD_CONFIG_DIR</key>"));
        assert!(plist.contains("<string>run</string>"));
    }

    #[test]
    fn the_unit_restarts_on_failure() {
        let unit = systemd_unit(
            Path::new("/usr/local/bin/acd"),
            Path::new("/home/u/.config/acd"),
        );
        assert!(unit.contains("ExecStart=\"/usr/local/bin/acd\" run"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("Environment=\"ACD_CONFIG_DIR=/home/u/.config/acd\""));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("# acd-managed:v1"));
        assert!(!unit.contains("CLAUDE_CONFIG_DIR"));
    }

    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<String>>,
        fail: Option<&'static str>,
    }

    impl Runner for Fake {
        fn run(&self, program: &str, args: &[String]) -> Result<()> {
            let line = format!("{program} {}", args.join(" "));
            self.calls.borrow_mut().push(line.clone());
            if self.fail.is_some_and(|word| line.contains(word)) {
                bail!("{line} failed (exit status: 5)");
            }
            Ok(())
        }
    }

    fn definition_path(home: &Path) -> PathBuf {
        if cfg!(target_os = "macos") {
            home.join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist"))
        } else {
            home.join(".config/systemd/user/acd.service")
        }
    }

    #[test]
    fn a_foreign_definition_is_refused_without_force() {
        let home = tempfile::tempdir().unwrap();
        let path = definition_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "someone else's service\n").unwrap();
        let fake = Fake::default();
        let error = install(
            home.path(),
            Path::new("/x/acd"),
            Path::new("/c"),
            false,
            false,
            &fake,
        )
        .unwrap_err();
        assert!(error.to_string().contains("--force"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "someone else's service\n"
        );
        assert!(fake.calls.borrow().is_empty());
        install(
            home.path(),
            Path::new("/x/acd"),
            Path::new("/c"),
            false,
            true,
            &fake,
        )
        .unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains(MARKER));
        let backup = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .find(|entry| entry.file_name().to_string_lossy().contains(".bak-"))
            .expect("backup");
        assert_eq!(
            std::fs::read_to_string(backup.path()).unwrap(),
            "someone else's service\n"
        );
    }

    #[test]
    fn reinstall_backs_up_and_reloads_a_running_service() {
        let home = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        install(
            home.path(),
            Path::new("/old/acd"),
            Path::new("/c"),
            false,
            false,
            &fake,
        )
        .unwrap();
        fake.calls.borrow_mut().clear();
        install(
            home.path(),
            Path::new("/new/acd"),
            Path::new("/c"),
            false,
            false,
            &fake,
        )
        .unwrap();
        assert!(
            std::fs::read_to_string(definition_path(home.path()))
                .unwrap()
                .contains("/new/acd")
        );
        let calls = fake.calls.borrow().join("\n");
        if cfg!(target_os = "macos") {
            assert!(
                calls.find("bootout").unwrap() < calls.find("bootstrap").unwrap(),
                "{calls}"
            );
        } else {
            for expected in ["daemon-reload", "enable acd.service", "restart acd.service"] {
                assert!(calls.contains(expected), "{expected}: {calls}");
            }
            assert!(calls.find("daemon-reload").unwrap() < calls.find("restart").unwrap());
        }
    }

    #[test]
    fn a_failing_runner_call_fails_install_and_names_the_call() {
        let home = tempfile::tempdir().unwrap();
        let word = if cfg!(target_os = "macos") {
            "bootstrap"
        } else {
            "restart"
        };
        let fake = Fake {
            fail: Some(word),
            ..Default::default()
        };
        let error = install(
            home.path(),
            Path::new("/x/acd"),
            Path::new("/c"),
            false,
            false,
            &fake,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains(word), "{error:#}");
    }
}
