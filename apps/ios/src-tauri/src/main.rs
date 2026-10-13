// Desktop entry point, so `cargo check` runs on the host. The iOS build calls
// `agent_caller_ios::run` from lib.rs through `mobile_entry_point`.
fn main() {
    agent_caller_ios::run()
}
