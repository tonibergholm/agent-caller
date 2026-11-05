//! Per-operation cost of the core's tables as they fill (in process).
use std::time::{Duration, Instant};

use acd_core::testing::{FakeAdapter, agent_obs, permission_obs, test_machine};
use acd_core::{Core, CorrelationKey, IdemCache, Observation};
use acd_protocol::{CommandResult, ResolvedBy};

pub fn run() {
    // One long-lived agent opening and resolving requests under unique keys.
    let core = Core::new(test_machine(), vec![FakeAdapter::new()]);
    core.observe(Observation::AgentUpsert(agent_obs("a1")));
    let mut t = Instant::now();
    println!("registry: one agent, open+resolve with unique keys");
    println!("{:>10} {:>12} {:>10}", "cycles", "us/cycle", "keys");
    for i in 1..=40_000u32 {
        let key = format!("occ-{i}");
        let mut obs = permission_obs("a1", &key);
        obs.reusable_key = false;
        core.observe(Observation::RequestSeen(obs));
        core.observe(Observation::RequestEnded { agent_id: "a1".into(), key: CorrelationKey(key), by: ResolvedBy::Inbox });
        if i % 5000 == 0 {
            println!("{:>10} {:>12.1} {:>10}", i, t.elapsed().as_secs_f64() * 1e6 / 5000.0, core.sizes().keys);
            t = Instant::now();
        }
    }
    // IdemCache::begin cost by fill (it scans every entry to expire old ones).
    println!("\nidempotency cache: begin()+finish() of one new command at a given fill");
    println!("{:>10} {:>12}", "entries", "us/command");
    let mut c = IdemCache::new(Duration::from_secs(900));
    let now = Instant::now();
    let mut n = 0u64;
    for target in [100, 1000, 4000, 8000, 12000, 16000] {
        while n < target {
            c.begin("c", &format!("id{n}"), [0; 32], now);
            c.finish("c", &format!("id{n}"), CommandResult::delivered("x"), true, now);
            n += 1;
        }
        let t = Instant::now();
        for j in 0..200 {
            let id = format!("probe{target}-{j}");
            c.begin("c", &id, [1; 32], now);
            c.finish("c", &id, CommandResult::delivered("x"), false, now);
        }
        println!("{:>10} {:>12.1}", target, t.elapsed().as_secs_f64() * 1e6 / 200.0);
    }
}
