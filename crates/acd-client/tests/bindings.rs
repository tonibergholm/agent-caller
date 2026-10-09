//! The frontend's TypeScript types are generated from Rust (decision D5).
//! Run with UPDATE_BINDINGS=1 to rewrite app/src/lib/bindings.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use acd_client::{
    settings::EndpointConfig,
    view::{InboxView, MachineEntryView},
};
use acd_protocol::ResolveReason;
use ts_rs::{Config, TS};

fn committed_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/src/lib/bindings")
}

fn read_dir(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "ts") {
                out.insert(
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    fs::read_to_string(&p).unwrap(),
                );
            }
        }
    }
    out
}

#[test]
fn bindings_are_current() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = Config::new()
        .with_large_int("number")
        .with_out_dir(tmp.path());
    InboxView::export_all(&cfg).unwrap();
    MachineEntryView::export_all(&cfg).unwrap();
    EndpointConfig::export_all(&cfg).unwrap();
    ResolveReason::export_all(&cfg).unwrap();
    let generated = read_dir(tmp.path());
    assert!(
        generated.contains_key("Agent.ts") && generated.contains_key("InboxView.ts"),
        "{:?}",
        generated.keys()
    );
    let dir = committed_dir();
    if std::env::var_os("UPDATE_BINDINGS").is_some() {
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (name, body) in &generated {
            fs::write(dir.join(name), body).unwrap();
        }
    }
    assert_eq!(
        read_dir(&dir),
        generated,
        "bindings drift; run UPDATE_BINDINGS=1 cargo test -p acd-client --test bindings and commit"
    );
}
