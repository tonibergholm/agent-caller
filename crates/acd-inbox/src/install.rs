//! `acd-inbox install | uninstall | status` (omarchy spec §10).
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

pub const UNIT_NAME: &str = "acd-inbox.service";
pub const PLUGIN_ID: &str = "tonibergholm.agentcaller";
pub const MARKER: &str = "Written by acd-inbox install";
/// Super+Alt+A, in the Lua form Omarchy reads from `bindings.lua`
/// (docs/research/2026-10-12-omarchy-plugin-api.md §11, overriding plan D22).
pub const KEYBINDING: &str = "o.bind(\"SUPER + ALT + A\", \"Agent inbox\", \"omarchy-shell tonibergholm.agentcaller toggle\")";

/// The plugin, embedded so install needs no checkout (plan D21).
pub const PLUGIN_FILES: &[(&str, &str)] = &[
    ("manifest.json", include_str!("../../../clients/omarchy/manifest.json")),
    ("BarWidget.qml", include_str!("../../../clients/omarchy/BarWidget.qml")),
    ("Panel.qml", include_str!("../../../clients/omarchy/Panel.qml")),
    ("Service.qml", include_str!("../../../clients/omarchy/Service.qml")),
    ("model.mjs", include_str!("../../../clients/omarchy/model.mjs")),
];

pub trait Runner {
    fn run(&self, program: &str, args: &[&str]) -> Result<()>;
    /// Waits between retries; tests replace it so they do not sleep.
    fn pause(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const ENABLE_ATTEMPTS: usize = 10;
const ENABLE_PAUSE: Duration = Duration::from_millis(300);

/// The last 300 characters of a tool's stderr, trimmed. This is local tool
/// output, not agent text.
pub fn stderr_tail(raw: &str) -> String {
    let t = raw.trim();
    let n = t.chars().count();
    if n <= 300 { t.to_owned() } else { format!("...{}", t.chars().skip(n - 300).collect::<String>()) }
}

/// Runs a program with stdout discarded and stderr kept for the error.
pub struct SystemRunner;

impl SystemRunner {
    pub fn run_with_timeout(&self, program: &str, args: &[&str], timeout: Duration) -> Result<()> {
        use std::io::Read;
        let mut child = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("running {program}"))?;
        let mut pipe = child.stderr.take().context("stderr pipe")?;
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        });
        let deadline = std::time::Instant::now() + timeout;
        let status = loop {
            if let Some(st) = child.try_wait()? {
                break Some(st);
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let err = reader.join().unwrap_or_default();
        match status {
            Some(st) if st.success() => Ok(()),
            Some(st) => bail!("{program} {} failed ({st}): {}", args.join(" "), stderr_tail(&err)),
            None => bail!("{program} {} timed out after {}s: {}", args.join(" "), timeout.as_secs(), stderr_tail(&err)),
        }
    }
}

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<()> {
        self.run_with_timeout(program, args, COMMAND_TIMEOUT)
    }
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub home: PathBuf,
    pub config: PathBuf,
}

impl Layout {
    pub fn from_env() -> Result<Layout> {
        let home = std::env::var_os("HOME").map(PathBuf::from).context("HOME is not set")?;
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Ok(Layout { home, config })
    }
    pub fn bin(&self) -> PathBuf {
        self.home.join(".local/bin/acd-inbox")
    }
    pub fn unit(&self) -> PathBuf {
        self.config.join("systemd/user").join(UNIT_NAME)
    }
    pub fn plugin_dir(&self) -> PathBuf {
        self.config.join("omarchy/plugins").join(PLUGIN_ID)
    }
    pub fn bindings(&self) -> PathBuf {
        self.config.join("hypr/bindings.lua")
    }
}

/// Quoted for systemd: `%` specifiers and `$` expansion escaped.
fn unit_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%").replace('$', "$$"))
}

/// The user unit (spec §4.2).
pub fn unit_text(exe: &Path) -> String {
    format!(
        "# {MARKER}\n[Unit]\nDescription=agent-caller inbox (acd-inbox)\nPartOf=graphical-session.target\nAfter=graphical-session.target\n\n[Service]\nExecStart={} serve\nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=graphical-session.target\n",
        unit_quote(&exe.to_string_lossy())
    )
}

fn write_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let dir = path.parent().context("the path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension("acd-inbox-new");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

/// `omarchy plugin enable`, retried while the shell's rescan finishes.
fn enable_plugin(runner: &dyn Runner) -> Result<()> {
    let mut last = None;
    for attempt in 0..ENABLE_ATTEMPTS {
        if attempt > 0 {
            runner.pause(ENABLE_PAUSE);
        }
        match runner.run("omarchy", &["plugin", "enable", PLUGIN_ID]) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
    }
    Err(last.expect("at least one attempt")).context(format!(
        "the plugin files are installed but `omarchy plugin enable {PLUGIN_ID}` failed {ENABLE_ATTEMPTS} times; run it by hand once the shell has rescanned"
    ))
}

fn ours(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|t| t.contains(MARKER))
}

/// Steps 1 to 3 of spec §10.
pub fn install(
    l: &Layout,
    exe: &Path,
    runner: &dyn Runner,
    ask: &mut dyn FnMut(&str) -> bool,
    out: &mut dyn Write,
) -> Result<()> {
    let bin = l.bin();
    let unit = l.unit();
    if is_symlink(&l.plugin_dir()) {
        bail!("{} is a symlink; remove it first", l.plugin_dir().display());
    }
    if unit.exists() && !ours(&unit) {
        bail!("{} exists and was not written by acd-inbox; remove it first", unit.display());
    }
    let same = std::fs::canonicalize(exe).ok().is_some_and(|e| std::fs::canonicalize(&bin).ok() == Some(e));
    if !same {
        let bytes = std::fs::read(exe).with_context(|| format!("reading {}", exe.display()))?;
        write_file(&bin, &bytes, 0o755)?;
        writeln!(out, "Installed {}", bin.display())?;
    }
    write_file(&unit, unit_text(&bin).as_bytes(), 0o644)?;
    runner.run("systemctl", &["--user", "daemon-reload"])?;
    runner.run("systemctl", &["--user", "enable", "--now", UNIT_NAME])?;
    // enable --now does not restart a running service (plan D27).
    runner.run("systemctl", &["--user", "restart", UNIT_NAME])?;
    writeln!(out, "Started {UNIT_NAME}")?;
    let dir = l.plugin_dir();
    for (name, text) in PLUGIN_FILES {
        write_file(&dir.join(name), text.as_bytes(), 0o644)?;
    }
    runner.run("omarchy-shell", &["shell", "rescanPlugins"])?;
    // rescanPlugins only schedules the scan, so the id can be unknown for a moment.
    enable_plugin(runner)?;
    writeln!(out, "Installed the plugin in {}", dir.display())?;
    writeln!(out, "Restart the shell to load the plugin's QML: omarchy-restart-shell (not done for you).")?;
    writeln!(out, "Keybinding for Super+Alt+A (add to {}):\n{KEYBINDING}", l.bindings().display())?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    offer_keybinding(&l.bindings(), ask, out, stamp)?;
    Ok(())
}

/// Appends the binding only after a yes, keeping a backup (spec §10).
pub fn offer_keybinding(path: &Path, ask: &mut dyn FnMut(&str) -> bool, out: &mut dyn Write, stamp: u64) -> Result<bool> {
    let Ok(existing) = std::fs::read_to_string(path) else {
        writeln!(out, "{} does not exist; add the line above to your Hyprland bindings (create the file if you want it).", path.display())?;
        return Ok(false);
    };
    if existing.lines().any(|l| l.trim() == KEYBINDING) {
        writeln!(out, "The keybinding is already in {}.", path.display())?;
        return Ok(false);
    }
    if !ask(&format!("Append it to {}? A backup is kept. [y/N] ", path.display())) {
        writeln!(out, "{} was not changed.", path.display())?;
        return Ok(false);
    }
    let name = path.file_name().context("bindings file name")?.to_string_lossy();
    let backup = path.with_file_name(format!("{name}.acd-inbox-{stamp}.bak"));
    std::fs::copy(path, &backup).with_context(|| format!("backing up {}", path.display()))?;
    let mut f = std::fs::OpenOptions::new().append(true).open(path)?;
    let sep = if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" };
    write!(f, "{sep}\n-- agent-caller inbox\n{KEYBINDING}\n")?;
    writeln!(out, "Added. Backup: {}", backup.display())?;
    Ok(true)
}

/// Reverses steps 1 and 2 of install; prints how to remove the binding.
/// A unit or plugin directory it did not create is left alone.
pub fn uninstall(l: &Layout, runner: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let unit = l.unit();
    if !unit.exists() || ours(&unit) {
        let _ = runner.run("systemctl", &["--user", "disable", "--now", UNIT_NAME]);
        if unit.exists() {
            std::fs::remove_file(&unit)?;
        }
        let _ = runner.run("systemctl", &["--user", "daemon-reload"]);
    } else {
        writeln!(out, "{} was not written by acd-inbox; left in place.", unit.display())?;
    }
    let _ = runner.run("omarchy", &["plugin", "disable", PLUGIN_ID]);
    let dir = l.plugin_dir();
    if is_symlink(&dir) {
        writeln!(out, "{} is a symlink; left in place.", dir.display())?;
    } else {
        for (name, _) in PLUGIN_FILES {
            let _ = std::fs::remove_file(dir.join(name));
        }
        let _ = std::fs::remove_dir(&dir);
    }
    let _ = runner.run("omarchy-shell", &["shell", "rescanPlugins"]);
    let bin = l.bin();
    if bin.exists() {
        std::fs::remove_file(&bin)?;
    }
    writeln!(out, "Removed {UNIT_NAME}, the plugin and {}.", bin.display())?;
    writeln!(out, "To remove the keybinding, delete this line from {}:\n{KEYBINDING}", l.bindings().display())?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub struct Probe {
    pub protocol: u64,
    pub service: String,
    /// Machines whose secret is not `ok`, counted by status.
    pub not_ok: std::collections::BTreeMap<String, usize>,
}

/// Connects as a view and reads `hello` and `settings`.
pub fn probe(socket: &Path, timeout: Duration) -> Result<Probe> {
    let s = std::os::unix::net::UnixStream::connect(socket).with_context(|| format!("connecting to {}", socket.display()))?;
    s.set_read_timeout(Some(timeout))?;
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let hello: serde_json::Value = serde_json::from_str(&line).context("reading hello")?;
    line.clear();
    r.read_line(&mut line)?;
    let settings: serde_json::Value = serde_json::from_str(&line).context("reading settings")?;
    let mut not_ok = std::collections::BTreeMap::new();
    for e in settings["machines"].as_array().into_iter().flatten() {
        let st = e["secret"].as_str().unwrap_or("unknown");
        if st != "ok" {
            *not_ok.entry(st.to_owned()).or_insert(0) += 1;
        }
    }
    Ok(Probe {
        protocol: hello["protocol"].as_u64().unwrap_or(0),
        service: hello["service"].as_str().unwrap_or("").to_owned(),
        not_ok,
    })
}

pub fn status(l: &Layout, socket: &Path, runner: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let active = runner.run("systemctl", &["--user", "is-active", "--quiet", UNIT_NAME]).is_ok();
    writeln!(out, "service: {}", if active { "running" } else { "not running" })?;
    match probe(socket, Duration::from_secs(2)) {
        Ok(p) => {
            writeln!(out, "socket: {} answers (protocol {}, service {})", socket.display(), p.protocol, p.service)?;
            if p.not_ok.is_empty() {
                writeln!(out, "keyring: ok")?;
            }
            for (st, n) in &p.not_ok {
                let hint = match st.as_str() {
                    "locked" => "unlock the keyring, then retry from the panel",
                    "missing" => "the secret is gone; pair the machine again",
                    "unreadable" => "the stored secret is damaged; pair the machine again",
                    _ => "the keyring did not answer; retry from the panel",
                };
                writeln!(out, "keyring: {n} machine(s) {st}: {hint}")?;
            }
        }
        Err(_) => {
            writeln!(out, "socket: {} does not answer", socket.display())?;
            writeln!(out, "keyring: unknown")?;
        }
    }
    let plugin = l.plugin_dir().join("manifest.json").exists();
    writeln!(out, "plugin: {}", if plugin { "installed" } else { "missing" })?;
    Ok(())
}

/// A y/N question: only a terminal and an explicit yes count. Anything else
/// (no terminal, end of input, any other word) is a no.
pub fn confirm(tty: bool, input: &mut dyn BufRead, out: &mut dyn Write, question: &str) -> bool {
    if !tty {
        return false;
    }
    let _ = write!(out, "{question}");
    let _ = out.flush();
    let mut answer = String::new();
    input.read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "Y" | "yes" | "Yes")
}

fn ask_on_terminal(question: &str) -> bool {
    // SAFETY: isatty only inspects the descriptor.
    let tty = unsafe { libc::isatty(0) } == 1;
    confirm(tty, &mut std::io::stdin().lock(), &mut std::io::stdout(), question)
}

fn linux_only(what: &str) -> Result<()> {
    if cfg!(target_os = "linux") { Ok(()) } else { bail!("acd-inbox {what} runs on Linux only") }
}

pub fn run_install() -> Result<()> {
    linux_only("install")?;
    let exe = std::env::current_exe()?;
    install(&Layout::from_env()?, &exe, &SystemRunner, &mut ask_on_terminal, &mut std::io::stdout())
}

pub fn run_uninstall() -> Result<()> {
    linux_only("uninstall")?;
    uninstall(&Layout::from_env()?, &SystemRunner, &mut std::io::stdout())
}

pub fn run_status() -> Result<()> {
    linux_only("status")?;
    let paths = crate::prefs::Paths::from_env()?;
    status(&Layout::from_env()?, &paths.socket, &SystemRunner, &mut std::io::stdout())
}