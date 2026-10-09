//! `~/.claude/sessions/<pid>.json` discovery (spec §6.2, gate 5). Read
//! only `<digits>.json`; never open the `.key` files. Best effort: an
//! entry that cannot be read or verified is skipped and logged once.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use acd_protocol::AgentState;
use serde_json::Value;

use super::wire::valid_id;

const MAX_ENTRY_BYTES: u64 = 64 * 1024;
/// Owner-only writes: a group- or world-writable entry is not trusted.
const MAX_ENTRY_MODE: u32 = 0o755;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryEntry {
    pub pid: i32,
    pub session_id: String,
    pub cwd: Option<String>,
    pub started_at_ms: Option<i64>,
    pub proc_start: Option<String>,
    pub entrypoint: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub waiting_for: Option<String>,
}

impl RegistryEntry {
    pub fn from_value(v: &Value) -> Option<RegistryEntry> {
        let o = v.as_object()?;
        let s = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_owned);
        let pid = o
            .get("pid")?
            .as_i64()
            .filter(|p| *p > 0 && *p <= i32::MAX as i64)? as i32;
        Some(RegistryEntry {
            pid,
            session_id: s("sessionId").filter(|x| valid_id(x))?,
            cwd: s("cwd"),
            started_at_ms: o.get("startedAt").and_then(Value::as_i64),
            proc_start: s("procStart"),
            entrypoint: s("entrypoint"),
            name: s("name"),
            status: s("status"),
            waiting_for: s("waitingFor"),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcVerdict {
    Live,
    Gone,
    Unverifiable,
}

pub trait ProcCheck: Send + Sync {
    fn check(&self, pid: i32, proc_start: &str) -> ProcVerdict;
}

/// `ps -o lstart=` in UTC (gate 5): `Fri Oct  9 07:29:36 2026`.
pub fn parse_lstart_utc(s: &str) -> Option<i64> {
    let t: Vec<&str> = s.split_whitespace().collect();
    let [_, mon, day, hms, year] = t.as_slice() else {
        return None;
    };
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS.iter().position(|m| m == mon)? as u32 + 1;
    let mut hms = hms.split(':').map(|x| x.parse::<u32>().ok());
    let (h, m, sec) = (hms.next()??, hms.next()??, hms.next()??);
    let date = chrono::NaiveDate::from_ymd_opt(year.parse().ok()?, month, day.parse().ok()?)?;
    Some(date.and_hms_opt(h, m, sec)?.and_utc().timestamp())
}

pub struct SystemProc;

impl ProcCheck for SystemProc {
    fn check(&self, pid: i32, proc_start: &str) -> ProcVerdict {
        let Some(claimed) = parse_lstart_utc(proc_start) else {
            return ProcVerdict::Unverifiable;
        };
        match process_start_secs(pid) {
            Ok(Some(actual)) if (actual - claimed).abs() <= 1 => ProcVerdict::Live,
            Ok(_) => ProcVerdict::Gone,
            Err(()) => ProcVerdict::Unverifiable,
        }
    }
}

#[cfg(target_os = "macos")]
fn pid_exists(pid: i32) -> Option<bool> {
    // SAFETY: signal 0 only checks for existence and permission.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return Some(true);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => Some(false),
        Some(libc::EPERM) => Some(true),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn process_start_secs(pid: i32) -> Result<Option<i64>, ()> {
    match pid_exists(pid) {
        Some(false) => return Ok(None),
        None => return Err(()),
        Some(true) => {}
    }
    // SAFETY: proc_bsdinfo is plain old data; all-zero is a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `info` is a properly sized, writable proc_bsdinfo.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if n == size {
        Ok(Some(info.pbi_start_tvsec as i64))
    } else {
        Err(())
    }
}

#[cfg(target_os = "linux")]
fn process_start_secs(pid: i32) -> Result<Option<i64>, ()> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    // Field 22 (starttime, clock ticks after boot); fields after the ")"
    // that ends the command name start at field 3.
    let after = stat.rsplit_once(')').ok_or(())?.1;
    let ticks: i64 = after
        .split_whitespace()
        .nth(19)
        .ok_or(())?
        .parse()
        .map_err(|_| ())?;
    let btime: i64 = std::fs::read_to_string("/proc/stat")
        .map_err(|_| ())?
        .lines()
        .find_map(|l| l.strip_prefix("btime "))
        .ok_or(())?
        .trim()
        .parse()
        .map_err(|_| ())?;
    // SAFETY: sysconf has no preconditions.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz <= 0 {
        return Err(());
    }
    Ok(Some(btime + ticks / hz as i64))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_start_secs(_pid: i32) -> Result<Option<i64>, ()> {
    Err(())
}

pub struct Scan {
    pub live: Vec<RegistryEntry>,
    /// False when any root or any entry could not be read or verified. An
    /// incomplete scan never removes an agent (spec §6.2) and never
    /// retires a hook token (T4).
    pub complete: bool,
    pub scanned_at: tokio::time::Instant,
    /// Wall-clock milliseconds when the scan started and finished (T4).
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
}

pub struct Scanner {
    dirs: Vec<PathBuf>,
    proc: Arc<dyn ProcCheck>,
    logged: HashSet<String>,
}

impl Scanner {
    pub fn new(dirs: Vec<PathBuf>, proc: Arc<dyn ProcCheck>) -> Scanner {
        Scanner {
            dirs,
            proc,
            logged: HashSet::new(),
        }
    }

    pub fn set_dirs(&mut self, dirs: Vec<PathBuf>) {
        self.dirs = dirs;
    }

    pub fn scan(&mut self) -> Scan {
        let started_at_ms = wall_ms();
        let mut live: Vec<RegistryEntry> = Vec::new();
        let mut complete = true;
        for dir in self.dirs.clone() {
            let (found, ok) = self.scan_dir(&dir);
            complete &= ok;
            for e in found {
                if !live.iter().any(|l| l.session_id == e.session_id) {
                    live.push(e);
                }
            }
        }
        Scan {
            live,
            complete,
            scanned_at: tokio::time::Instant::now(),
            started_at_ms,
            finished_at_ms: wall_ms(),
        }
    }

    fn log_once(&mut self, what: &str, why: &str) {
        if self.logged.insert(format!("{what}:{why}")) {
            tracing::info!(entry = %what, %why, "skipping a Claude session registry entry");
        }
    }

    fn scan_dir(&mut self, dir: &Path) -> (Vec<RegistryEntry>, bool) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => {
                self.log_once(&dir.display().to_string(), "unreadable");
                return (vec![], false);
            }
        };
        let mut live = Vec::new();
        let mut complete = true;
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let entry =
                acd_securefs::read_private(&dir.join(name), MAX_ENTRY_MODE, MAX_ENTRY_BYTES)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                    .and_then(|v| RegistryEntry::from_value(&v))
                    .filter(|e| e.pid.to_string() == stem);
            let Some(entry) = entry else {
                complete = false;
                self.log_once(name, "unreadable");
                continue;
            };
            match entry
                .proc_start
                .as_deref()
                .map(|p| self.proc.check(entry.pid, p))
            {
                Some(ProcVerdict::Live) => live.push(entry),
                Some(ProcVerdict::Gone) => {}
                Some(ProcVerdict::Unverifiable) | None => {
                    complete = false;
                    self.log_once(name, "process metadata unverifiable");
                }
            }
        }
        (live, complete)
    }
}

pub fn map_status(status: Option<&str>) -> AgentState {
    match status {
        Some("idle") => AgentState::Idle,
        Some("busy") => AgentState::Working,
        Some("waiting") => AgentState::NeedsYou,
        _ => AgentState::Unknown,
    }
}

/// A complete scan as the token retirement needs it (T4).
#[derive(Clone, Debug)]
pub struct CompleteScan {
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub live: Vec<RegistryEntry>,
}

pub fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

pub fn sessions_dir(home: &Path, claude_config_dir: Option<&Path>) -> PathBuf {
    claude_config_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".claude"))
        .join("sessions")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fixture_entry() -> Value {
        let p = format!(
            "{}/../../tests/fixtures/hooks/claude/session-registry-entry.json",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    struct Fixed(ProcVerdict);
    impl ProcCheck for Fixed {
        fn check(&self, _: i32, _: &str) -> ProcVerdict {
            self.0
        }
    }

    fn write(dir: &Path, name: &str, body: &str) {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn the_recorded_entry_parses() {
        let e = RegistryEntry::from_value(&fixture_entry()).unwrap();
        assert_eq!(
            (e.pid, e.status.as_deref(), e.entrypoint.as_deref()),
            (14517, Some("idle"), Some("sdk-ts"))
        );
        assert_eq!(e.proc_start.as_deref(), Some("Fri Oct  9 07:29:36 2026"));
    }

    #[test]
    fn lstart_is_utc() {
        // 2026-10-09T07:29:36Z
        assert_eq!(
            parse_lstart_utc("Fri Oct  9 07:29:36 2026"),
            Some(1_791_530_976)
        );
        assert_eq!(
            parse_lstart_utc("Fri Oct 19 07:29:36 2026"),
            Some(1_791_530_976 + 10 * 86_400)
        );
        assert_eq!(parse_lstart_utc("yesterday"), None);
        assert_eq!(parse_lstart_utc("Fri Foo  9 07:29:36 2026"), None);
    }

    #[test]
    fn statuses_map_and_unknown_is_unknown() {
        assert_eq!(map_status(Some("idle")), AgentState::Idle);
        assert_eq!(map_status(Some("busy")), AgentState::Working);
        assert_eq!(map_status(Some("waiting")), AgentState::NeedsYou);
        assert_eq!(map_status(Some("frobnicating")), AgentState::Unknown);
        assert_eq!(map_status(None), AgentState::Unknown);
        let seq = std::fs::read_to_string(format!(
            "{}/../../tests/fixtures/hooks/claude/session-registry-status-sequence.jsonl",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        for line in seq.lines() {
            let v: Value = serde_json::from_str(line).unwrap();
            assert_ne!(
                map_status(v["status"].as_str()),
                AgentState::Unknown,
                "{line}"
            );
        }
    }

    #[test]
    fn scan_reads_only_pid_json_and_skips_what_it_cannot_verify() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = fixture_entry();
        write(dir.path(), "14517.json", &e.to_string());
        write(dir.path(), "14517.0123abcd.key", "SECRET");
        write(dir.path(), "notes.json", "{}");
        e["pid"] = 222.into();
        e["sessionId"] = "s-222".into();
        write(dir.path(), "999.json", &e.to_string()); // pid does not match the name
        write(dir.path(), "333.json", "{\"pid\": 333, \"sess"); // truncated
        let mut s = Scanner::new(vec![dir.path().into()], Arc::new(Fixed(ProcVerdict::Live)));
        let scan = s.scan();
        assert_eq!(
            scan.live.iter().map(|e| e.pid).collect::<Vec<_>>(),
            vec![14517]
        );
        assert!(
            !scan.complete,
            "an unreadable entry makes the scan incomplete"
        );
        let mut gone = Scanner::new(vec![dir.path().into()], Arc::new(Fixed(ProcVerdict::Gone)));
        assert!(
            gone.scan().live.is_empty(),
            "a kill -9 leaves the entry; the pid check drops it"
        );
        let mut unverifiable = Scanner::new(
            vec![dir.path().into()],
            Arc::new(Fixed(ProcVerdict::Unverifiable)),
        );
        assert!(unverifiable.scan().live.is_empty());
    }

    #[test]
    fn an_unreadable_directory_is_an_incomplete_empty_scan() {
        let mut s = Scanner::new(
            vec!["/nonexistent/acd-test".into()],
            Arc::new(Fixed(ProcVerdict::Live)),
        );
        let scan = s.scan();
        assert!(scan.live.is_empty() && !scan.complete);
    }

    #[test]
    fn a_world_writable_entry_is_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "14517.json", &fixture_entry().to_string());
        std::fs::set_permissions(
            dir.path().join("14517.json"),
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        let mut s = Scanner::new(vec![dir.path().into()], Arc::new(Fixed(ProcVerdict::Live)));
        assert!(s.scan().live.is_empty());
    }

    #[test]
    fn the_system_check_recognises_this_process() {
        let pid = std::process::id() as i32;
        let out = std::process::Command::new("ps")
            .env("TZ", "UTC")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let lstart = String::from_utf8(out.stdout).unwrap();
        assert_eq!(
            SystemProc.check(pid, lstart.trim()),
            ProcVerdict::Live,
            "{lstart}"
        );
        assert_eq!(
            SystemProc.check(pid, "Thu Jan  1 00:00:00 2015"),
            ProcVerdict::Gone,
            "pid reuse"
        );
        assert_eq!(
            SystemProc.check(0x7fff_fff0, lstart.trim()),
            ProcVerdict::Gone
        );
        assert_eq!(SystemProc.check(pid, "garbage"), ProcVerdict::Unverifiable);
    }

    #[test]
    fn several_roots_are_scanned_and_deduplicated() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        write(a.path(), "14517.json", &fixture_entry().to_string());
        write(b.path(), "14517.json", &fixture_entry().to_string());
        let mut e = fixture_entry();
        e["pid"] = 15000.into();
        e["sessionId"] = "s-other".into();
        write(b.path(), "15000.json", &e.to_string());
        let mut s = Scanner::new(
            vec![a.path().into(), b.path().into()],
            Arc::new(Fixed(ProcVerdict::Live)),
        );
        let scan = s.scan();
        let mut ids: Vec<_> = scan.live.iter().map(|e| e.session_id.clone()).collect();
        ids.sort();
        assert_eq!(
            ids.len(),
            2,
            "the same session in two roots counts once: {ids:?}"
        );
        assert!(scan.complete);
        assert!(
            scan.started_at_ms > 0 && scan.started_at_ms <= scan.finished_at_ms,
            "wall-clock times for T4"
        );
        s.set_dirs(vec![a.path().into(), "/nonexistent/acd-test".into()]);
        assert!(
            !s.scan().complete,
            "one unreadable root makes the scan incomplete"
        );
    }

    #[test]
    fn sessions_dir_honours_claude_config_dir() {
        assert_eq!(
            sessions_dir(Path::new("/h"), None),
            PathBuf::from("/h/.claude/sessions")
        );
        assert_eq!(
            sessions_dir(Path::new("/h"), Some(Path::new("/c"))),
            PathBuf::from("/c/sessions")
        );
    }
}
