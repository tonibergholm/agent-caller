mod common;
use common::*;
use std::time::{Duration, Instant};

const STATE: &str = "/external/v1/producers/sup/state";

fn body(key: &str) -> String {
    format!(
        r#"{{"lease_ms":60000,"items":[{{"key":"{key}","agent_key":"a1","agent":{{"title":"autopilot"}},"title":"ENG-12 parked","options":[{{"id":"approve","label":"Approve"}}]}}]}}"#
    )
}

fn status(dir: &std::path::Path) -> String {
    let out = acd(dir).arg("status").output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_producer_api_is_served_on_hook_sock_and_never_on_tcp() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = init_dir(tmp.path());
    let started = start_daemon(&dir, None, "", &[]);
    let token = store_token(&dir);
    let sock = dir.join("hook.sock");

    let unknown = call_hook(started.hook_port, "PUT", "/nowhere", &token, &body("k1"));
    assert_eq!(unknown.0, 401);
    let tcp = call_hook(started.hook_port, "PUT", STATE, &token, &body("k1"));
    assert_eq!(
        tcp, unknown,
        "TCP answers /external like any unknown route, even with the right token"
    );
    assert!(
        !status(&dir).contains("autopilot"),
        "the TCP call must not have created an agent"
    );

    assert!(
        status(&dir).contains("external=true"),
        "status reports the producer API: {}",
        status(&dir)
    );
    assert_eq!(
        call_hook_unix(&sock, "PUT", STATE, "wrong", &body("k1")).0,
        401
    );
    let (code, reply) = call_hook_unix(&sock, "PUT", STATE, &token, &body("k1"));
    assert_eq!(code, 200, "{reply}");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !status(&dir).contains("autopilot") {
        assert!(
            Instant::now() < deadline,
            "the agent never appeared: {}",
            status(&dir)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        call_hook_unix(&sock, "DELETE", "/external/v1/producers/sup", &token, "").0,
        204
    );
    assert!(!status(&dir).contains("autopilot"));
}

#[test]
fn the_hook_routes_still_work_on_both_listeners() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = init_dir(tmp.path());
    let started = start_daemon(&dir, None, "", &[]);
    let token = store_token(&dir);
    let path = "/hook/claude/UserPromptSubmit";
    assert_eq!(
        post_hook(started.hook_port, path, "wrong", &[], "{}").0,
        401
    );
    assert_eq!(post_hook(started.hook_port, path, &token, &[], "{}").0, 200);
    assert_eq!(
        post_hook_unix(&dir.join("hook.sock"), path, &token, &[], "{}").0,
        200
    );
}
