//! `acd service install`: launchd on macOS and a systemd user unit on Linux.
//! Both restart acd after any non-zero exit and leave clean stops alone.
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

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

/// The path acd was invoked by when it is absolute, else the resolved
/// executable. Canonicalizing would pin a Homebrew keg path
/// (`/opt/homebrew/Cellar/acd/<version>/bin/acd`) that disappears on upgrade;
/// the invoked `/opt/homebrew/bin/acd` symlink keeps working.
pub fn invoked_exe() -> Result<PathBuf> {
    if let Some(arg0) = std::env::args_os().next() {
        let arg0 = PathBuf::from(arg0);
        if arg0.is_absolute() && arg0.exists() {
            return Ok(arg0);
        }
    }
    Ok(std::fs::canonicalize(std::env::current_exe()?)?)
}

fn contains_marker(bytes: &[u8]) -> bool {
    let marker = MARKER.as_bytes();
    bytes.windows(marker.len()).any(|window| window == marker)
}

const MAX_SERVICE_BACKUPS: usize = 5;

fn service_backup_timestamp(name: &str, service_name: &str) -> Option<(i64, u32)> {
    let suffix = name.strip_prefix(&format!("{service_name}.bak-"))?;
    let mut parts = suffix.split('-');
    let stamp = parts.next()?.parse().ok()?;
    let attempt = match parts.next() {
        Some(attempt) => attempt.parse().ok()?,
        None => 0,
    };
    (parts.next().is_none()).then_some((stamp, attempt))
}

fn prune_service_backups(backup_dir: &Path, service_name: &str) -> Result<()> {
    let mut backups = Vec::new();
    for entry in std::fs::read_dir(backup_dir)
        .with_context(|| format!("reading {}", backup_dir.display()))?
    {
        let entry = entry.with_context(|| format!("reading {}", backup_dir.display()))?;
        if !entry
            .file_type()
            .with_context(|| format!("reading {}", entry.path().display()))?
            .is_file()
        {
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(timestamp) = service_backup_timestamp(name, service_name) else {
            continue;
        };
        backups.push((timestamp, entry.path()));
    }
    backups.sort_by_key(|(timestamp, _)| *timestamp);
    for (_, backup) in backups.into_iter().rev().skip(MAX_SERVICE_BACKUPS) {
        std::fs::remove_file(&backup).with_context(|| format!("removing {}", backup.display()))?;
    }
    Ok(())
}

fn backup_definition(path: &Path, old: &[u8], backup_dir: &Path) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::create_dir_all(backup_dir)
        .with_context(|| format!("creating {}", backup_dir.display()))?;
    let name = path
        .file_name()
        .expect("service path has a name")
        .to_string_lossy();
    let stamp = chrono::Utc::now().timestamp_millis();
    for attempt in 0u32.. {
        let file = if attempt == 0 {
            format!("{name}.bak-{stamp}")
        } else {
            format!("{name}.bak-{stamp}-{attempt}")
        };
        let backup = backup_dir.join(file);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&backup)
        {
            Ok(mut out) => {
                out.write_all(old)
                    .with_context(|| format!("writing backup {}", backup.display()))?;
                prune_service_backups(backup_dir, &name)?;
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("creating {}", backup.display()));
            }
        }
    }
    unreachable!()
}

fn write_definition(path: &Path, text: &str, force: bool, backup_dir: &Path) -> Result<bool> {
    std::fs::create_dir_all(path.parent().expect("service path has parent"))?;
    let had_definition = match std::fs::read(path) {
        Ok(old) => {
            if !contains_marker(&old) && !force {
                bail!(
                    "{} exists and was not written by acd; rerun with --force to replace it (a backup is kept)",
                    path.display()
                );
            }
            // Identical content needs no backup, but the rewrite below still
            // restores mode 0o644.
            if old != text.as_bytes() {
                backup_definition(path, &old, backup_dir)?;
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

/// Quotes for systemd and escapes specifiers (`%`); `dollar` also escapes `$`
/// (variable expansion applies to ExecStart only).
fn unit_quote(s: &str, dollar: bool) -> String {
    let mut out = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    if dollar {
        out = out.replace('$', "$$");
    }
    format!("\"{out}\"")
}

pub fn systemd_unit(exe: &Path, config_dir: &Path) -> String {
    format!(
        "# {MARKER}\n[Unit]\nDescription=agent-caller daemon (acd)\n\n[Service]\nExecStart={} run\nEnvironment={}\nEnvironment=\"PATH={PATH_LINUX}\"\nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n",
        unit_quote(&exe.to_string_lossy(), true),
        unit_quote(&format!("ACD_CONFIG_DIR={}", config_dir.display()), false),
    )
}

/// How long `install` waits for launchd to finish a `bootout`, and how it
/// retries a `bootstrap` that still loses the race.
#[derive(Clone, Copy)]
pub struct Timing {
    pub unload_poll: std::time::Duration,
    pub unload_polls: u32,
    pub bootstrap_backoff: std::time::Duration,
    pub bootstrap_retries: u32,
}

impl Timing {
    pub const LIVE: Timing = Timing {
        unload_poll: std::time::Duration::from_millis(100),
        unload_polls: 50,
        bootstrap_backoff: std::time::Duration::from_millis(250),
        bootstrap_retries: 4,
    };
}

pub fn install(
    home: &Path,
    exe: &Path,
    config_dir: &Path,
    print: bool,
    force: bool,
    runner: &dyn Runner,
) -> Result<String> {
    install_with(home, exe, config_dir, print, force, runner, Timing::LIVE)
}

pub fn install_with(
    home: &Path,
    exe: &Path,
    config_dir: &Path,
    print: bool,
    force: bool,
    runner: &dyn Runner,
    timing: Timing,
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
        let existed = write_definition(&plist, &text, force, &config_dir.join("backups"))?;
        let uid = acd_securefs::euid();
        let job = format!("gui/{uid}/{LABEL}");
        if existed {
            // A service that is not loaded (stopped by hand, failed bootstrap,
            // foreign plist) makes bootout fail; bootstrap is the real check.
            match runner.run("launchctl", &["bootout".into(), job.clone()]) {
                Err(error) => tracing::debug!("launchctl bootout ignored: {error:#}"),
                // bootout returns before launchd has finished unloading, and a
                // bootstrap that races it fails with EIO. Wait until the job is gone.
                Ok(()) => {
                    for _ in 0..timing.unload_polls {
                        if runner
                            .run("launchctl", &["print".into(), job.clone()])
                            .is_err()
                        {
                            break;
                        }
                        std::thread::sleep(timing.unload_poll);
                    }
                }
            }
        }
        let bootstrap = [
            "bootstrap".to_string(),
            format!("gui/{uid}"),
            plist.display().to_string(),
        ];
        let mut retries = timing.bootstrap_retries;
        loop {
            match runner.run("launchctl", &bootstrap) {
                Ok(()) => break,
                // Exit status 5 (EIO) is launchd still tearing the old job down.
                Err(error) if retries > 0 && format!("{error:#}").contains("exit status: 5") => {
                    retries -= 1;
                    tracing::debug!("launchctl bootstrap retrying: {error:#}");
                    std::thread::sleep(timing.bootstrap_backoff);
                }
                Err(error) => return Err(error.context("launchctl bootstrap failed")),
            }
        }
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
        write_definition(&unit, &text, force, &config_dir.join("backups"))?;
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
            "Installed and started {}. Stop it with: systemctl --user disable --now acd.service. To keep it running after you log out: loginctl enable-linger $USER",
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
        /// `launchctl print` succeeds (job still loaded) this many more times.
        print_present: std::cell::Cell<u32>,
        /// `launchctl bootstrap` fails with exit status 5 this many more times.
        bootstrap_eio: std::cell::Cell<u32>,
    }

    const FAST: Timing = Timing {
        unload_poll: std::time::Duration::from_millis(1),
        unload_polls: 20,
        bootstrap_backoff: std::time::Duration::from_millis(1),
        bootstrap_retries: 3,
    };

    impl Runner for Fake {
        fn run(&self, program: &str, args: &[String]) -> Result<()> {
            let line = format!("{program} {}", args.join(" "));
            self.calls.borrow_mut().push(line.clone());
            if line.starts_with("launchctl print ") {
                if self.print_present.get() > 0 {
                    self.print_present.set(self.print_present.get() - 1);
                    return Ok(());
                }
                bail!("{line} failed (exit status: 113)");
            }
            if line.starts_with("launchctl bootstrap ") && self.bootstrap_eio.get() > 0 {
                self.bootstrap_eio.set(self.bootstrap_eio.get() - 1);
                bail!("{line} failed (exit status: 5)");
            }
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

    fn run_install(home: &Path, cfg: &Path, exe: &str, force: bool, fake: &Fake) -> Result<String> {
        install_with(home, Path::new(exe), cfg, false, force, fake, FAST)
    }

    fn backups(cfg: &Path) -> Vec<PathBuf> {
        match std::fs::read_dir(cfg.join("backups")) {
            Ok(dir) => dir.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
            Err(_) => Vec::new(),
        }
    }

    #[test]
    fn a_foreign_definition_is_refused_without_force() {
        let home = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let path = definition_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "someone else's service\n").unwrap();
        let fake = Fake::default();
        let error = run_install(home.path(), cfg.path(), "/x/acd", false, &fake).unwrap_err();
        assert!(error.to_string().contains("--force"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "someone else's service\n"
        );
        assert!(fake.calls.borrow().is_empty());
        run_install(home.path(), cfg.path(), "/x/acd", true, &fake).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains(MARKER));
        let found = backups(cfg.path());
        assert_eq!(found.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&found[0]).unwrap(),
            "someone else's service\n"
        );
        // Nothing is left beside the definition.
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn a_non_utf8_foreign_definition_can_be_forced() {
        let home = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let path = definition_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, [0xff, 0xfe, b'x', 0x80]).unwrap();
        let fake = Fake::default();
        assert!(run_install(home.path(), cfg.path(), "/x/acd", false, &fake).is_err());
        run_install(home.path(), cfg.path(), "/x/acd", true, &fake).unwrap();
        let found = backups(cfg.path());
        assert_eq!(found.len(), 1);
        assert_eq!(std::fs::read(&found[0]).unwrap(), [0xff, 0xfe, b'x', 0x80]);
    }

    #[test]
    fn reinstall_backs_up_and_reloads_a_running_service() {
        let home = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        run_install(home.path(), cfg.path(), "/old/acd", false, &fake).unwrap();
        fake.calls.borrow_mut().clear();
        run_install(home.path(), cfg.path(), "/new/acd", false, &fake).unwrap();
        assert!(std::fs::read_to_string(definition_path(home.path()))
            .unwrap()
            .contains("/new/acd"));
        let found = backups(cfg.path());
        assert_eq!(found.len(), 1);
        assert!(std::fs::read_to_string(&found[0])
            .unwrap()
            .contains("/old/acd"));
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
    fn reinstall_keeps_only_five_service_backups() {
        let home = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let backup_dir = cfg.path().join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let unrelated = backup_dir.join("keep-me.txt");
        std::fs::write(&unrelated, "unrelated backup").unwrap();
        let fake = Fake::default();

        run_install(home.path(), cfg.path(), "/acd/0", false, &fake).unwrap();
        for version in 1..=7 {
            run_install(
                home.path(),
                cfg.path(),
                &format!("/acd/{version}"),
                false,
                &fake,
            )
            .unwrap();
        }

        let service_path = definition_path(home.path());
        let service_name = service_path.file_name().unwrap().to_str().unwrap();
        let service_backups = backups(cfg.path())
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| service_backup_timestamp(name, service_name))
                    .is_some()
            })
            .count();
        assert_eq!(service_backups, 5);
        assert_eq!(
            std::fs::read_to_string(unrelated).unwrap(),
            "unrelated backup"
        );
    }

    #[test]
    fn an_identical_reinstall_writes_no_backup_and_restores_the_mode() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        run_install(home.path(), cfg.path(), "/x/acd", false, &fake).unwrap();
        run_install(home.path(), cfg.path(), "/x/acd", false, &fake).unwrap();
        assert!(backups(cfg.path()).is_empty());
        let path = definition_path(home.path());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        run_install(home.path(), cfg.path(), "/x/acd", false, &fake).unwrap();
        assert!(backups(cfg.path()).is_empty());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
    }

    #[test]
    fn reinstall_succeeds_when_bootout_fails_because_nothing_is_loaded() {
        let home = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let ok = Fake::default();
        run_install(home.path(), cfg.path(), "/old/acd", false, &ok).unwrap();
        let fake = Fake {
            fail: Some("bootout"),
            ..Default::default()
        };
        run_install(home.path(), cfg.path(), "/new/acd", false, &fake).unwrap();
        if cfg!(target_os = "macos") {
            let calls = fake.calls.borrow().join("\n");
            assert!(calls.contains("bootout"), "{calls}");
            assert!(calls.contains("bootstrap"), "{calls}");
        }
    }

    #[test]
    fn the_unit_escapes_specifiers_and_variables() {
        let unit = systemd_unit(Path::new("/opt/50%/a$b/acd"), Path::new("/home/u/100%/c$d"));
        assert!(
            unit.contains("ExecStart=\"/opt/50%%/a$$b/acd\" run"),
            "{unit}"
        );
        assert!(
            unit.contains("Environment=\"ACD_CONFIG_DIR=/home/u/100%%/c$d\""),
            "{unit}"
        );
        assert!(!unit.contains("network-online"));
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
        let cfg = tempfile::tempdir().unwrap();
        let error = run_install(home.path(), cfg.path(), "/x/acd", false, &fake).unwrap_err();
        assert!(format!("{error:#}").contains(word), "{error:#}");
    }

    fn reinstall_over_loaded(fake: &Fake) -> Result<String> {
        let home = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        run_install(home.path(), cfg.path(), "/old/acd", false, &Fake::default()).unwrap();
        run_install(home.path(), cfg.path(), "/new/acd", false, fake)
    }

    #[test]
    fn bootstrap_waits_until_the_booted_out_job_is_gone() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let fake = Fake::default();
        fake.print_present.set(2);
        reinstall_over_loaded(&fake).unwrap();
        let calls = fake.calls.borrow();
        let prints = calls.iter().filter(|c| c.contains(" print ")).count();
        assert_eq!(prints, 3, "{calls:?}");
        let last_print = calls.iter().rposition(|c| c.contains(" print ")).unwrap();
        let boots: Vec<_> = calls
            .iter()
            .enumerate()
            .filter(|(_, c)| c.contains(" bootstrap "))
            .collect();
        assert_eq!(boots.len(), 1, "{calls:?}");
        assert!(boots[0].0 > last_print, "{calls:?}");
        assert!(calls[0].contains("bootout"), "{calls:?}");
    }

    #[test]
    fn bootstrap_retries_an_eio_failure() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let fake = Fake::default();
        fake.bootstrap_eio.set(1);
        reinstall_over_loaded(&fake).unwrap();
        let boots = fake
            .calls
            .borrow()
            .iter()
            .filter(|c| c.contains(" bootstrap "))
            .count();
        assert_eq!(boots, 2);
    }

    #[test]
    fn bootstrap_gives_up_after_bounded_retries_and_names_the_call() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let fake = Fake::default();
        fake.bootstrap_eio.set(u32::MAX);
        let error = reinstall_over_loaded(&fake).unwrap_err();
        assert!(
            format!("{error:#}").contains("launchctl bootstrap"),
            "{error:#}"
        );
        let boots = fake
            .calls
            .borrow()
            .iter()
            .filter(|c| c.contains(" bootstrap "))
            .count();
        assert_eq!(boots, 1 + FAST.bootstrap_retries as usize);
    }

    #[test]
    fn a_non_eio_bootstrap_failure_is_not_retried() {
        if !cfg!(target_os = "macos") {
            return;
        }
        struct Other(RefCell<u32>);
        impl Runner for Other {
            fn run(&self, _: &str, args: &[String]) -> Result<()> {
                if args[0] == "bootstrap" {
                    *self.0.borrow_mut() += 1;
                    bail!("launchctl bootstrap failed (exit status: 37)");
                }
                Ok(())
            }
        }
        let home = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let other = Other(RefCell::new(0));
        assert!(install_with(
            home.path(),
            Path::new("/x/acd"),
            cfg.path(),
            false,
            false,
            &other,
            FAST
        )
        .is_err());
        assert_eq!(*other.0.borrow(), 1);
    }
}
