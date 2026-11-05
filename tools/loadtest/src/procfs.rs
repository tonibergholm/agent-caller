//! Process sampling from /proc (Linux).
use std::fs;

pub struct Sample {
    pub rss_kb: u64,
    pub hwm_kb: u64,
    pub threads: u64,
    pub fds: u64,
    /// utime + stime, in clock ticks.
    pub cpu_ticks: u64,
}

pub fn clk_tck() -> f64 {
    // SAFETY: sysconf has no preconditions.
    unsafe { libc::sysconf(libc::_SC_CLK_TCK) as f64 }
}

pub fn sample(pid: u32) -> Option<Sample> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let field = |name: &str| -> u64 {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|r| r.trim_start_matches(':').split_whitespace().next())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    let fds = fs::read_dir(format!("/proc/{pid}/fd")).ok()?.count() as u64;
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is in parentheses and may hold spaces: split after the last ')'.
    let rest = stat.rsplit_once(')')?.1;
    let parts: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = parts.get(11)?.parse().ok()?;
    let stime: u64 = parts.get(12)?.parse().ok()?;
    Some(Sample {
        rss_kb: field("VmRSS"),
        hwm_kb: field("VmHWM"),
        threads: field("Threads"),
        fds,
        cpu_ticks: utime + stime,
    })
}
