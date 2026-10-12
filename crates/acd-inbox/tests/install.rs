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
}

impl Runner for Recorder {
    fn run(&self, program: &str, args: &[&str]) -> Result<()> {
        let call = format!("{program} {}", args.join(" "));
        self.calls.borrow_mut().push(call.clone());
        if self.fail.is_some_and(|f| call.contains(f)) {
            bail!("{call} failed");
        }
        Ok(())
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
