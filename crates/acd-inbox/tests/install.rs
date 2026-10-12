mod common;

use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use acd_inbox::install::*;
use anyhow::{Result, bail};
use common::*;

#[derive(Default)]
struct Recorder {
    calls: RefCell<Vec<String>>,
    fail: Option<&'static str>,
    /// Fails the first N `plugin enable` calls.
    enable_fails: std::cell::Cell<usize>,
    pauses: std::cell::Cell<usize>,
    /// Calls to these programs report as inactive/failing for `is-active`.
    inactive: bool,
}

impl Runner for Recorder {
    fn run(&self, program: &str, args: &[&str]) -> Result<()> {
        let call = format!("{program} {}", args.join(" "));
        self.calls.borrow_mut().push(call.clone());
        if call.contains("plugin enable") && self.enable_fails.get() > 0 {
            self.enable_fails.set(self.enable_fails.get() - 1);
            bail!("plugin {PLUGIN_ID} is not known");
        }
        if self.inactive && call.contains("is-active") {
            bail!("inactive");
        }
        if self.fail.is_some_and(|f| call.contains(f)) {
            bail!("{call} failed");
        }
        Ok(())
    }
    fn pause(&self, _: std::time::Duration) {
        self.pauses.set(self.pauses.get() + 1);
    }
}

fn layout(home: &Path) -> Layout {
    Layout { home: home.to_path_buf(), config: home.join(".config") }
}

fn build(dir: &Path) -> std::path::PathBuf {
    let exe = dir.join("acd-inbox-build");
    std::fs::write(&exe, "binary").unwrap();
    exe
}

#[test]
fn the_unit_follows_the_graphical_session_and_quotes_the_path() {
    let unit = unit_text(Path::new("/home/u/50%/a$b/.local/bin/acd-inbox"));
    assert!(unit.contains("ExecStart=\"/home/u/50%%/a$$b/.local/bin/acd-inbox\" serve"));
    for line in [
        "PartOf=graphical-session.target",
        "After=graphical-session.target",
        "WantedBy=graphical-session.target",
        "Restart=on-failure",
        "RestartSec=2",
    ] {
        assert!(unit.lines().any(|l| l == line), "{line}");
    }
    assert!(unit.starts_with(&format!("# {MARKER}")));
}

#[test]
fn install_copies_the_binary_writes_the_unit_and_plugin_and_runs_the_steps_in_order() {
    let home = tempfile::tempdir().unwrap();
    let l = layout(home.path());
    let runner = Recorder::default();
    let mut asked = Vec::new();
    let mut out = Vec::new();
    install(&l, &build(home.path()), &runner, &mut |q| { asked.push(q.to_owned()); false }, &mut out).unwrap();
    assert_eq!(std::fs::read_to_string(l.bin()).unwrap(), "binary");
    assert_eq!(std::fs::metadata(l.bin()).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(std::fs::read_to_string(l.unit()).unwrap(), unit_text(&l.bin()));
    for (name, text) in PLUGIN_FILES {
        assert_eq!(std::fs::read_to_string(l.plugin_dir().join(name)).unwrap(), *text, "{name}");
    }
    assert_eq!(
        *runner.calls.borrow(),
        [
            "systemctl --user daemon-reload",
            "systemctl --user enable --now acd-inbox.service",
            "systemctl --user restart acd-inbox.service",
            "omarchy-shell shell rescanPlugins",
            "omarchy plugin enable tonibergholm.agentcaller",
        ]
    );
    let printed = String::from_utf8(out).unwrap();
    assert!(printed.contains(KEYBINDING));
    assert!(printed.contains("omarchy-restart-shell"), "{printed}");
    assert!(!runner.calls.borrow().iter().any(|c| c.contains("restart-shell")), "never restarts the shell");
    assert!(asked.is_empty(), "no bindings file, so nothing to ask");
}

#[test]
fn a_unit_install_did_not_write_is_never_replaced() {
    let home = tempfile::tempdir().unwrap();
    let l = layout(home.path());
    std::fs::create_dir_all(l.unit().parent().unwrap()).unwrap();
    std::fs::write(l.unit(), "[Service]\nExecStart=/mine\n").unwrap();
    let runner = Recorder::default();
    assert!(install(&l, &build(home.path()), &runner, &mut |_| false, &mut Vec::new()).is_err());
    assert_eq!(std::fs::read_to_string(l.unit()).unwrap(), "[Service]\nExecStart=/mine\n");
    assert!(runner.calls.borrow().is_empty());
}

#[test]
fn the_keybinding_changes_nothing_without_a_yes_and_keeps_a_backup() {
    let d = tempfile::tempdir().unwrap();
    let bindings = d.path().join("bindings.lua");
    std::fs::write(&bindings, "-- personal overrides\no.bind(\"SUPER + RETURN\", \"Terminal\", \"xdg-terminal-exec\")").unwrap();
    let before = std::fs::read_to_string(&bindings).unwrap();
    assert!(!offer_keybinding(&bindings, &mut |_| false, &mut Vec::new(), 1).unwrap());
    assert_eq!(std::fs::read_to_string(&bindings).unwrap(), before);
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1, "no backup without a yes");
    assert!(offer_keybinding(&bindings, &mut |_| true, &mut Vec::new(), 2).unwrap());
    let after = std::fs::read_to_string(&bindings).unwrap();
    assert!(after.starts_with(&before));
    assert_eq!(after.lines().filter(|l| *l == KEYBINDING).count(), 1);
    let backup = d.path().join("bindings.lua.acd-inbox-2.bak");
    assert_eq!(std::fs::read_to_string(backup).unwrap(), before);
    let mut asked = false;
    assert!(!offer_keybinding(&bindings, &mut |_| { asked = true; true }, &mut Vec::new(), 3).unwrap());
    assert!(!asked, "an existing binding is not offered again");
    assert!(!offer_keybinding(&d.path().join("missing.conf"), &mut |_| true, &mut Vec::new(), 4).unwrap());
    assert!(!d.path().join("missing.conf").exists());
}

#[test]
fn uninstall_reverses_install_and_prints_the_binding_to_remove() {
    let home = tempfile::tempdir().unwrap();
    let l = layout(home.path());
    install(&l, &build(home.path()), &Recorder::default(), &mut |_| false, &mut Vec::new()).unwrap();
    let runner = Recorder::default();
    let mut out = Vec::new();
    uninstall(&l, &runner, &mut out).unwrap();
    assert!(!l.bin().exists() && !l.unit().exists() && !l.plugin_dir().exists());
    assert!(runner.calls.borrow().contains(&"systemctl --user disable --now acd-inbox.service".to_owned()));
    assert!(String::from_utf8(out).unwrap().contains(KEYBINDING));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reports_service_socket_keyring_and_plugin() {
    let rig = rig(vec![pending_agent("a1", "r1")], fast()).await;
    let home = tempfile::tempdir().unwrap();
    let l = layout(home.path());
    let socket = rig.socket.clone();
    let printed = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        status(&l, &socket, &Recorder::default(), &mut out).unwrap();
        String::from_utf8(out).unwrap()
    })
    .await
    .unwrap();
    assert!(printed.contains("service: running"), "{printed}");
    assert!(printed.contains("answers (protocol 1"), "{printed}");
    assert!(printed.contains("keyring: ok"), "{printed}");
    assert!(printed.contains("plugin: missing"), "{printed}");
}

#[test]
fn a_question_is_answered_only_on_a_terminal_with_an_explicit_yes() {
    let ask = |tty: bool, input: &str| confirm(tty, &mut input.as_bytes(), &mut Vec::new(), "q? ");
    assert!(ask(true, "y\n") && ask(true, "yes\n") && ask(true, "Y\n"));
    assert!(!ask(true, "\n") && !ask(true, "no\n") && !ask(true, ""));
    assert!(!ask(false, "y\n"), "non-interactive stdin is never a yes");
}

#[test]
fn install_with_a_declined_binding_leaves_bindings_lua_untouched() {
    let home = tempfile::tempdir().unwrap();
    let l = layout(home.path());
    std::fs::create_dir_all(l.bindings().parent().unwrap()).unwrap();
    std::fs::write(l.bindings(), "-- mine\n").unwrap();
    let mut asked = 0;
    install(&l, &build(home.path()), &Recorder::default(), &mut |_| { asked += 1; false }, &mut Vec::new()).unwrap();
    assert_eq!(asked, 1);
    assert_eq!(std::fs::read_to_string(l.bindings()).unwrap(), "-- mine\n");
    assert_eq!(std::fs::read_dir(l.bindings().parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn the_keybinding_is_the_recorded_lua_line() {
    assert_eq!(KEYBINDING, "o.bind(\"SUPER + ALT + A\", \"Agent inbox\", \"omarchy-shell tonibergholm.agentcaller toggle\")");
    assert_eq!(layout(Path::new("/h")).bindings(), Path::new("/h/.config/hypr/bindings.lua"));
}

fn ok_install(l: &Layout, home: &Path) {
    install(l, &build(home), &Recorder::default(), &mut |_| false, &mut Vec::new()).unwrap();
}

#[test]
fn enable_is_retried_while_the_shell_rescans() {
    let home = tempfile::tempdir().unwrap();
    let l = layout(home.path());
    let runner = Recorder { enable_fails: 3.into(), ..Default::default() };
    install(&l, &build(home.path()), &runner, &mut |_| false, &mut Vec::new()).unwrap();
    let enables = runner.calls.borrow().iter().filter(|c| c.contains("plugin enable")).count();
    assert_eq!(enables, 4);
    assert_eq!(runner.pauses.get(), 3);
}

#[test]
fn enable_that_never_succeeds_stops_with_a_clear_error() {
    let home = tempfile::tempdir().unwrap();
    let l = layout(home.path());
    let runner = Recorder { enable_fails: 1000.into(), ..Default::default() };
    let err = install(&l, &build(home.path()), &runner, &mut |_| false, &mut Vec::new()).unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("failed 10 times") && text.contains("not known"), "{text}");
    assert_eq!(runner.calls.borrow().iter().filter(|c| c.contains("plugin enable")).count(), 10);
}

#[test]
fn a_failing_command_reports_the_tail_of_its_stderr() {
    let err = SystemRunner.run("sh", &["-c", "echo boom-detail >&2; exit 3"]).unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("boom-detail") && text.contains("failed"), "{text}");
    let long = "x".repeat(1000) + "END";
    let tail = stderr_tail(&format!("  {long}\n"));
    assert!(tail.ends_with("END") && tail.len() < 320, "{}", tail.len());
}

#[test]
fn a_command_that_hangs_is_killed_at_the_timeout() {
    let started = std::time::Instant::now();
    let err = SystemRunner.run_with_timeout("sleep", &["5"], std::time::Duration::from_millis(200)).unwrap_err();
    assert!(format!("{err:#}").contains("timed out"));
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
}

#[test]
fn uninstall_keeps_foreign_files_and_a_foreign_unit() {
    let home = tempfile::tempdir().unwrap();
    let l = layout(home.path());
    ok_install(&l, home.path());
    std::fs::write(l.plugin_dir().join("mine.txt"), "x").unwrap();
    std::fs::create_dir_all(l.bindings().parent().unwrap()).unwrap();
    std::fs::write(l.bindings(), format!("-- mine\n{KEYBINDING}\n")).unwrap();
    uninstall(&l, &Recorder::default(), &mut Vec::new()).unwrap();
    assert!(l.plugin_dir().join("mine.txt").exists());
    assert!(!l.plugin_dir().join("manifest.json").exists());
    assert!(std::fs::read_to_string(l.bindings()).unwrap().contains(KEYBINDING));
}

#[test]
fn uninstall_does_not_disable_a_unit_it_did_not_write() {
    let home = tempfile::tempdir().unwrap();
    let l = layout(home.path());
    std::fs::create_dir_all(l.unit().parent().unwrap()).unwrap();
    std::fs::write(l.unit(), "[Service]\nExecStart=/mine\n").unwrap();
    let runner = Recorder::default();
    let mut out = Vec::new();
    uninstall(&l, &runner, &mut out).unwrap();
    assert!(l.unit().exists());
    assert!(!runner.calls.borrow().iter().any(|c| c.contains("disable --now") || c.contains("daemon-reload")));
    assert!(String::from_utf8(out).unwrap().contains("not written by acd-inbox"));
}

#[test]
fn a_symlinked_plugin_dir_is_neither_installed_into_nor_emptied() {
    let home = tempfile::tempdir().unwrap();
    let l = layout(home.path());
    let target = home.path().join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("manifest.json"), "keep").unwrap();
    std::fs::create_dir_all(l.plugin_dir().parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, l.plugin_dir()).unwrap();
    let runner = Recorder::default();
    assert!(install(&l, &build(home.path()), &runner, &mut |_| false, &mut Vec::new()).is_err());
    assert!(runner.calls.borrow().is_empty());
    assert_eq!(std::fs::read_to_string(target.join("manifest.json")).unwrap(), "keep");
    uninstall(&l, &Recorder::default(), &mut Vec::new()).unwrap();
    assert_eq!(std::fs::read_to_string(target.join("manifest.json")).unwrap(), "keep");
    assert!(l.plugin_dir().exists());
}

/// A stand-in service: sends hello and the given settings, then closes.
fn fake_service(dir: &Path, machines: serde_json::Value) -> std::path::PathBuf {
    let path = dir.join("fake.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    std::thread::spawn(move || {
        use std::io::Write;
        if let Ok((mut s, _)) = listener.accept() {
            let _ = writeln!(s, "{}", serde_json::json!({"type":"hello","protocol":1,"service":"t"}));
            let _ = writeln!(s, "{}", serde_json::json!({"type":"settings","machines":machines}));
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    });
    path
}

fn status_text(l: &Layout, socket: &Path, runner: &Recorder) -> String {
    let mut out = Vec::new();
    status(l, socket, runner, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn status_names_each_unreadable_secret_status_with_its_own_advice() {
    let home = tempfile::tempdir().unwrap();
    let l = layout(home.path());
    let socket = fake_service(
        home.path(),
        serde_json::json!([{"secret":"locked"},{"secret":"missing"},{"secret":"ok"}]),
    );
    let printed = status_text(&l, &socket, &Recorder::default());
    assert!(printed.contains("1 machine(s) locked: unlock the keyring"), "{printed}");
    assert!(printed.contains("1 machine(s) missing: the secret is gone; pair the machine again"), "{printed}");
    assert!(!printed.contains("keyring: ok"), "{printed}");
}

#[test]
fn status_with_nothing_listening_and_the_service_down() {
    let home = tempfile::tempdir().unwrap();
    let l = layout(home.path());
    let runner = Recorder { inactive: true, ..Default::default() };
    let printed = status_text(&l, &home.path().join("none.sock"), &runner);
    assert!(printed.contains("service: not running"), "{printed}");
    assert!(printed.contains("does not answer"), "{printed}");
    assert!(printed.contains("keyring: unknown"), "{printed}");
    assert!(printed.contains("plugin: missing"), "{printed}");
}

#[test]
fn a_grandchild_holding_stderr_cannot_stall_the_runner() {
    let started = std::time::Instant::now();
    let err = SystemRunner
        .run_with_timeout("sh", &["-c", "sleep 30 & echo err-from-child >&2; exit 1"], std::time::Duration::from_secs(5))
        .unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(3), "{:?}", started.elapsed());
    assert!(format!("{err:#}").contains("err-from-child"));
}

#[test]
fn a_noisy_command_keeps_only_the_end_of_its_stderr() {
    let err = SystemRunner
        .run_with_timeout("sh", &["-c", "yes noise | head -c 300000 >&2; echo LAST >&2; exit 1"], std::time::Duration::from_secs(5))
        .unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("LAST") && text.len() < 1000, "{}", text.len());
}
