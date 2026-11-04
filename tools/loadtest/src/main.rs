//! Load and soak harness for acd. See README.md.
mod args;
mod clients;
mod daemon;
mod host;
mod hist;
mod http;
mod limits;
mod metrics;
mod procfs;
mod producers;
mod rng;
mod soak;

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = argv.first().cloned() else {
        eprintln!("usage: acd-loadtest soak|host|limits [--key value ...]  (see README.md)");
        std::process::exit(2);
    };
    let a = args::Args::parse(&argv[1..]);
    let r = match cmd.as_str() {
        "soak" => soak::run(a).await,
        "host" => host::run(host::HostArgs {
            dir: a.str("dir", "/tmp/acd-loadtest-host").into(),
            agents: a.usize("agents", 128),
            rate: a.f64("rate", 8.0),
            ack_delay_ms: a.f64("ack-delay-ms", 5.0) as u64,
            withdraw_after_s: a.f64("withdraw-after", 15.0),
            turnover: a.f64("turnover", 0.02),
            burst_extra: a.usize("burst-extra", 0),
            detail_bytes: a.usize("detail-bytes", 0),
            open_at_start: a.usize("open-at-start", 0) == 1,
        })
        .await,
        "limits" => limits::run(a).await,
        other => Err(format!("unknown command {other}")),
    };
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
