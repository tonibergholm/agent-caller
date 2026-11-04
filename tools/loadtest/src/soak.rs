//! The orchestrator: starts the process under test, the fake agents and
//! the clients, samples everything every interval, restarts and bursts on
//! a schedule, and writes the CSV and a summary.
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use crate::args::Args;
use crate::clients::{self, ClientCfg};
use crate::daemon::{Proc, Target};
use crate::metrics::{Metrics, Windows};
use crate::procfs;
use crate::producers::{self, ProducerCfg};

const HEADER: &str = "t_s,pid,rss_kb,hwm_kb,threads,fds,cpu_pct,log_bytes,\
opens_s,seen_s,answers_ok_s,resolved_s,\
vis_n,vis_p50_ms,vis_p99_ms,vis_max_ms,res_n,res_p50_ms,res_p99_ms,res_max_ms,cmd_p50_ms,cmd_p99_ms,\
clients_online,errors,lagged,conn_ends,drops,restarts,welcome_same,welcome_new,\
agents,requests,keys,seen,idem,ring,receivers,tasks";

fn ms(us: u64) -> String {
    format!("{:.2}", us as f64 / 1000.0)
}

fn sizes(dir: &std::path::Path) -> [String; 8] {
    let v: serde_json::Value = std::fs::read_to_string(dir.join("sizes.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);
    let g = |k: &str| v[k].as_u64().map_or(String::new(), |n| n.to_string());
    [g("agents"), g("requests"), g("keys"), g("seen"), g("idem"), g("ring"), g("receivers"), g("tasks")]
}

pub async fn run(a: Args) -> Result<(), String> {
    let host = a.str("target", "acd") == "host";
    let work = PathBuf::from(a.str("work", "/tmp/acd-loadtest"));
    let _ = std::fs::remove_dir_all(&work);
    let dir = work.join("cfg");
    let duration = Duration::from_secs_f64(a.f64("duration", 600.0));
    let interval = Duration::from_secs_f64(a.f64("sample", 10.0));
    let agents = a.usize("agents", 128);
    let rate = a.f64("rate", 8.0);
    let restart_every = a.f64("restart-every", 0.0);
    let burst_every = a.f64("burst-every", 0.0);
    let csv_path = PathBuf::from(a.str("csv", "soak.csv"));
    let summary_path = PathBuf::from(a.str("summary", "soak-summary.txt"));

    let target = if host {
        Target::Host {
            args: vec![
                "--agents".into(), agents.to_string(),
                "--rate".into(), rate.to_string(),
                "--ack-delay-ms".into(), a.str("ack-delay-ms", "5"),
                "--withdraw-after".into(), a.str("withdraw-after", "15"),
                "--turnover".into(), a.str("turnover", "0.02"),
                "--burst-extra".into(), a.str("burst-extra", "0"),
            ],
        }
    } else {
        Target::Acd { bin: PathBuf::from(a.str("acd-bin", "../../target/release/acd")) }
    };
    let mut proc = Proc::new(target, dir.clone());
    proc.init()?;
    proc.start().await?;

    let m = Arc::new(Metrics::default());
    let (stop_tx, stop_rx) = watch::channel(false);
    let burst = Arc::new(AtomicU64::new(0));
    if !host {
        let producers_n = agents.div_ceil(32).clamp(1, 8);
        producers::spawn_producers(
            &ProducerCfg {
                sock: proc.hook_sock(),
                token: proc.hook_token()?,
                producers: producers_n,
                slots: (agents / producers_n).clamp(1, 32),
                rate,
                withdraw_after_s: a.f64("withdraw-after", 15.0),
                turnover: a.f64("turnover", 0.02),
                p_odd_ack: a.f64("odd-ack", 0.03),
                run_id: format!("{:x}", crate::rng::now_us() & 0xffff_ffff),
                burst: burst.clone(),
            },
            m.clone(),
            stop_rx.clone(),
        );
    }
    let handles = clients::spawn_clients(
        &ClientCfg {
            dir: dir.clone(),
            count: a.usize("clients", 4),
            answer_min_s: a.f64("answer-min", 0.05),
            answer_max_s: a.f64("answer-max", 1.5),
            p_answer: a.f64("p-answer", 0.9),
            p_dup: a.f64("p-dup", 0.02),
            churn_every_s: a.f64("churn-every", 60.0),
            churn_frac: a.f64("churn-frac", 0.25),
        },
        m.clone(),
        stop_rx.clone(),
    );

    let mut csv = std::fs::File::create(&csv_path).map_err(|e| e.to_string())?;
    writeln!(csv, "{HEADER}").map_err(|e| e.to_string())?;
    let started = Instant::now();
    let mut w = Windows::default();
    let tck = procfs::clk_tck();
    let mut prev_cpu = proc.pid().and_then(procfs::sample).map_or(0, |s| s.cpu_ticks);
    let mut prev_pid = proc.pid();
    let mut prev = [0u64; 4];
    let mut next_restart = restart_every;
    let mut next_burst = burst_every;
    let mut restarts = 0u64;
    let mut peak_rss = 0u64;
    let mut rss_series: Vec<(f64, u64)> = Vec::new();
    let mut fd_series: Vec<(f64, u64)> = Vec::new();
    let mut last_tick = Instant::now();
    eprintln!("soak: {} s, target {}, sampling {} s -> {}", duration.as_secs(), if host { "host" } else { "acd" }, interval.as_secs(), csv_path.display());
    while started.elapsed() < duration {
        tokio::time::sleep(interval).await;
        let t = started.elapsed().as_secs_f64();
        let dt = last_tick.elapsed().as_secs_f64().max(0.001);
        last_tick = Instant::now();
        if restart_every > 0.0 && t >= next_restart {
            next_restart += restart_every;
            restarts += 1;
            Metrics::inc(&m.restarts);
            proc.stop(restarts % 2 == 0);
            tokio::time::sleep(Duration::from_millis(500)).await;
            proc.start().await?;
            prev_cpu = 0;
            prev_pid = None;
        }
        if burst_every > 0.0 && t >= next_burst {
            next_burst += burst_every;
            burst.fetch_add(1, Relaxed);
            if host && let Some(pid) = proc.pid() {
                // SAFETY: signalling the child this run started.
                unsafe { libc::kill(pid as i32, libc::SIGUSR1) };
            }
        }
        let pid = proc.pid();
        let s = pid.and_then(procfs::sample);
        if pid != prev_pid {
            prev_cpu = 0;
            prev_pid = pid;
        }
        let cpu_pct = s.as_ref().map_or(0.0, |s| {
            let d = s.cpu_ticks.saturating_sub(prev_cpu) as f64 / tck;
            d / dt * 100.0
        });
        if let Some(s) = &s {
            prev_cpu = s.cpu_ticks;
            peak_rss = peak_rss.max(s.rss_kb);
            rss_series.push((t, s.rss_kb));
            fd_series.push((t, s.fds));
        }
        w.take(&m);
        let cur = [
            Metrics::get(&m.opened_at_source),
            Metrics::get(&m.opened_seen),
            Metrics::get(&m.answers_ok),
            Metrics::get(&m.resolved_seen),
        ];
        let rate_of = |i: usize| (cur[i].saturating_sub(prev[i])) as f64 / dt;
        let errs: u64 = m.errors.lock().unwrap().values().sum();
        let sz = sizes(&dir);
        let log_bytes = std::fs::metadata(dir.join("process.log")).map_or(0, |m| m.len());
        let (v, r, c) = (&w.open_visible, &w.answer_resolved, &w.command_rtt);
        writeln!(
            csv,
            "{:.0},{},{},{},{},{},{:.1},{},{:.2},{:.2},{:.2},{:.2},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            t,
            pid.unwrap_or(0),
            s.as_ref().map_or(0, |s| s.rss_kb),
            s.as_ref().map_or(0, |s| s.hwm_kb),
            s.as_ref().map_or(0, |s| s.threads),
            s.as_ref().map_or(0, |s| s.fds),
            cpu_pct,
            log_bytes,
            rate_of(0), rate_of(1), rate_of(2), rate_of(3),
            v.count(), ms(v.quantile(0.5)), ms(v.quantile(0.99)), ms(v.max()),
            r.count(), ms(r.quantile(0.5)), ms(r.quantile(0.99)), ms(r.max()),
            ms(c.quantile(0.5)), ms(c.quantile(0.99)),
            clients::online(&handles),
            errs,
            Metrics::get(&m.lagged),
            Metrics::get(&m.connection_ends),
            Metrics::get(&m.drops_injected),
            restarts,
            Metrics::get(&m.welcomed_same),
            Metrics::get(&m.welcomed_new),
            sz.join(","),
        )
        .map_err(|e| e.to_string())?;
        csv.flush().ok();
        prev = cur;
    }
    let _ = stop_tx.send(true);
    for h in &handles {
        h.cancel();
    }
    let mut out = String::new();
    let line = |out: &mut String, k: &str, v: String| out.push_str(&format!("{k}={v}\n"));
    line(&mut out, "duration_s", format!("{:.0}", started.elapsed().as_secs_f64()));
    line(&mut out, "target", if host { "host" } else { "acd" }.into());
    for (name, h) in [("open_to_visible", &w.total_open_visible), ("answer_to_resolved", &w.total_answer_resolved), ("command_rtt", &w.total_command_rtt)] {
        line(&mut out, &format!("{name}_n"), h.count().to_string());
        line(&mut out, &format!("{name}_p50_ms"), ms(h.quantile(0.5)));
        line(&mut out, &format!("{name}_p99_ms"), ms(h.quantile(0.99)));
        line(&mut out, &format!("{name}_max_ms"), ms(h.max()));
    }
    for (k, c) in [
        ("opened_at_source", &m.opened_at_source), ("opened_seen_by_clients", &m.opened_seen),
        ("answers_sent", &m.answers_sent), ("answers_ok", &m.answers_ok), ("resolved_seen", &m.resolved_seen),
        ("withdrawn", &m.withdrawn), ("drops_injected", &m.drops_injected), ("welcomed_same", &m.welcomed_same),
        ("welcomed_new", &m.welcomed_new), ("connection_ends", &m.connection_ends), ("lagged", &m.lagged),
        ("restarts", &m.restarts), ("producer_errors", &m.producer_errors), ("producer_puts", &m.producer_puts),
    ] {
        line(&mut out, k, Metrics::get(c).to_string());
    }
    line(&mut out, "peak_rss_kb", peak_rss.to_string());
    line(&mut out, "rss_first_10pct_median_kb", median_of(&rss_series, 0.0, 0.1).to_string());
    line(&mut out, "rss_last_10pct_median_kb", median_of(&rss_series, 0.9, 1.0).to_string());
    line(&mut out, "fds_first_10pct_median", median_of(&fd_series, 0.0, 0.1).to_string());
    line(&mut out, "fds_last_10pct_median", median_of(&fd_series, 0.9, 1.0).to_string());
    line(&mut out, "errors", format!("{:?}", m.errors.lock().unwrap()));
    line(&mut out, "resolved_by_for_answered", format!("{:?}", m.resolved_by.lock().unwrap()));
    std::fs::write(&summary_path, &out).map_err(|e| e.to_string())?;
    print!("{out}");
    proc.stop(true);
    Ok(())
}

fn median_of(series: &[(f64, u64)], lo: f64, hi: f64) -> u64 {
    let Some(&(end, _)) = series.last() else { return 0 };
    let mut v: Vec<u64> = series.iter().filter(|(t, _)| *t >= end * lo && *t <= end * hi).map(|(_, x)| *x).collect();
    v.sort_unstable();
    v.get(v.len() / 2).copied().unwrap_or(0)
}
