mod common;

use std::sync::Arc;

use acd_relay::ManualClock;
use acd_relay::limits::Limit;
use acd_relay::server::Rates;
use common::*;
use serde_json::json;

async fn plain() -> (Relay, Arc<FakeSender>) {
    let s = Arc::new(FakeSender::default());
    (start(Setup::default(), s.clone()).await, s)
}

async fn plain_with(rates: Rates) -> (Relay, Arc<FakeSender>) {
    let s = Arc::new(FakeSender::default());
    (start(Setup { rates, ..Setup::default() }, s.clone()).await, s)
}

#[tokio::test]
async fn wake_sends_one_push_and_returns_204() {
    let (r, s) = plain().await;
    let h = r.make_handle(&s, "tok:one").await;
    assert_eq!(r.wake(&h).await, 204);
    assert_eq!(s.wakes.lock().unwrap().as_slice(), ["tok:one"]);
}

#[tokio::test]
async fn registration_end_to_end_then_wake() {
    let (r, s) = plain().await;
    let (st, b) = r.register("tok:e2e").await;
    assert_eq!(st, 202);
    assert_eq!(b["challenge_id"].as_str().unwrap().len(), 22);
    // The challenge went to the address, with a 22 character code.
    let (to, code) = s.challenges.lock().unwrap()[0].clone();
    assert_eq!(to, "tok:e2e");
    assert_eq!(code.len(), 22);
    let id = b["challenge_id"].as_str().unwrap();
    let ip = r.ip();
    let (st, b) = r.post("/v1/handles/confirm", &Relay::confirm_body(id, &code, "tok:e2e"), &ip).await;
    assert_eq!(st, 200);
    assert!(b["handle"].as_str().unwrap().len() <= 512);
    // 30 days from T0 as RFC 3339 with milliseconds.
    let want = chrono::DateTime::from_timestamp((T0 + 30 * 86400) as i64, 0)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%S.000Z")
        .to_string();
    assert_eq!(b["expires_at"], want.as_str());
    assert_eq!(r.wake(b["handle"].as_str().unwrap()).await, 204);
    assert_eq!(s.wake_count(), 1);
}

#[tokio::test]
async fn tampered_handle_is_400_and_sends_nothing() {
    let (r, s) = plain().await;
    let h = r.make_handle(&s, "tok:t").await;
    let mut raw = h.clone().into_bytes();
    let i = raw.len() / 2;
    raw[i] = if raw[i] == b'A' { b'B' } else { b'A' };
    let ip = r.ip();
    let (st, b) = r.post("/v1/wake", &json!({"v":1,"handle":String::from_utf8(raw).unwrap()}), &ip).await;
    assert_eq!((st, b["error"].as_str()), (400, Some("invalid_handle")));
    let (st, b) = r.post("/v1/wake", &json!({"v":1,"handle":"x"}), &ip).await;
    assert_eq!((st, b["error"].as_str()), (400, Some("invalid_handle")));
    assert_eq!(s.wake_count(), 0);
}

#[tokio::test]
async fn expired_handle_is_404_gone_and_sends_nothing() {
    let (r, s) = plain().await;
    let h = r.make_handle(&s, "tok:x").await;
    r.clock.advance(30 * 86400);
    let ip = r.ip();
    let (st, b) = r.post("/v1/wake", &json!({"v":1,"handle":h}), &ip).await;
    assert_eq!((st, b["error"].as_str()), (404, Some("gone")));
    assert_eq!(s.wake_count(), 0);
}

#[tokio::test]
async fn wake_with_wrong_version_or_extra_field_is_400() {
    let (r, s) = plain().await;
    let h = r.make_handle(&s, "tok:v").await;
    let ip = r.ip();
    assert_eq!(r.post("/v1/wake", &json!({"v":2,"handle":h}), &ip).await.0, 400);
    assert_eq!(r.post("/v1/wake", &json!({"v":1,"handle":h,"text":"approve"}), &ip).await.0, 400);
    assert_eq!(s.wake_count(), 0);
}

// ---- push service errors through the real FCM client ----

async fn with_fcm() -> (Relay, FakeFcm, Arc<FakeSender>) {
    let fake = fake_fcm().await;
    let clock = Arc::new(ManualClock::new(T0));
    let client = fcm_client(&fake, clock.clone());
    let r = start_clock(Setup::default(), client, clock).await;
    (r, fake, Arc::new(FakeSender::default()))
}

/// Registers through the real client; the challenge code is read from what
/// the fake FCM received.
async fn fcm_handle(r: &Relay, fake: &FakeFcm, address: &str) -> String {
    let (st, b) = r.register(address).await;
    assert_eq!(st, 202, "{b}");
    let code = fake.st.sends.lock().unwrap().last().unwrap().1["message"]["data"]["c"]
        .as_str()
        .unwrap()
        .to_string();
    let ip = r.ip();
    let (st, b) = r
        .post("/v1/handles/confirm", &Relay::confirm_body(b["challenge_id"].as_str().unwrap(), &code, address), &ip)
        .await;
    assert_eq!(st, 200);
    b["handle"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn push_gone_maps_to_404_gone() {
    let (r, fake, _) = with_fcm().await;
    let h = fcm_handle(&r, &fake, "tok:dead").await;
    *fake.st.send_reply.lock().unwrap() = (404, r#"{"error":{"code":404,"status":"UNREGISTERED"}}"#.into());
    let ip = r.ip();
    let (st, b) = r.post("/v1/wake", &json!({"v":1,"handle":h}), &ip).await;
    assert_eq!((st, b["error"].as_str()), (404, Some("gone")));
}

#[tokio::test]
async fn push_invalid_argument_does_not_map_to_gone() {
    let (r, fake, _) = with_fcm().await;
    let h = fcm_handle(&r, &fake, "tok:ia").await;
    *fake.st.send_reply.lock().unwrap() = (400, r#"{"error":{"code":400,"status":"INVALID_ARGUMENT"}}"#.into());
    let ip = r.ip();
    let (st, b) = r.post("/v1/wake", &json!({"v":1,"handle":h}), &ip).await;
    assert_eq!((st, b["error"].as_str()), (503, Some("unavailable")));
}

#[tokio::test]
async fn push_unavailable_maps_to_503() {
    let (r, fake, _) = with_fcm().await;
    let h = fcm_handle(&r, &fake, "tok:un").await;
    for (code, status) in [(503, "UNAVAILABLE"), (500, "INTERNAL"), (429, "QUOTA_EXCEEDED")] {
        *fake.st.send_reply.lock().unwrap() = (code, format!(r#"{{"error":{{"status":"{status}"}}}}"#));
        let ip = r.ip();
        let (st, b) = r.post("/v1/wake", &json!({"v":1,"handle":h}), &ip).await;
        assert_eq!((st, b["error"].as_str()), (503, Some("unavailable")), "{status}");
    }
    // An OAuth failure is also 503.
    *fake.st.token_reply.lock().unwrap() = (401, r#"{"error":"invalid_grant"}"#.into());
    *fake.st.send_reply.lock().unwrap() = (200, "{}".into());
    r.clock.advance(7200); // the cached access token has expired
    let ip = r.ip();
    let (st, b) = r.post("/v1/wake", &json!({"v":1,"handle":h}), &ip).await;
    assert_eq!((st, b["error"].as_str()), (503, Some("unavailable")), "oauth failure");
}

// ---- rate limits ----

#[tokio::test]
async fn wake_rate_limited_per_handle() {
    let (r, s) = plain().await;
    let h = r.make_handle(&s, "tok:ph").await;
    let mut codes = vec![];
    for _ in 0..12 {
        codes.push(r.wake(&h).await); // a fresh IP each time
    }
    assert_eq!(codes.iter().filter(|c| **c == 204).count(), 10);
    assert_eq!(codes[10..], [429, 429]);
    assert_eq!(s.wake_count(), 10);
    // Another handle is unaffected.
    let h2 = r.make_handle(&s, "tok:ph2").await;
    assert_eq!(r.wake(&h2).await, 204);
    // The window resets.
    r.clock.advance(61);
    assert_eq!(r.wake(&h).await, 204);
}

#[tokio::test]
async fn wake_rate_limited_per_address_across_two_handles() {
    let (r, s) = plain().await;
    let a = r.make_handle(&s, "tok:shared").await;
    let b = r.make_handle(&s, "tok:shared").await;
    assert_ne!(a, b);
    let mut ok = 0;
    for i in 0..12 {
        // Alternate: each handle alone stays at 6, below its own limit of 10.
        if r.wake(if i % 2 == 0 { &a } else { &b }).await == 204 {
            ok += 1;
        }
    }
    assert_eq!(ok, 10, "the shared address limit applies");
    assert_eq!(s.wake_count(), 10);
}

#[tokio::test]
async fn wake_rate_limited_per_ip() {
    let (r, s) = plain_with(Rates { wake_ip: Limit::hourly(5), ..Rates::default() }).await;
    let h = r.make_handle(&s, "tok:ip").await;
    let same = "203.0.113.7";
    let mut codes = vec![];
    for _ in 0..7 {
        codes.push(r.post("/v1/wake", &json!({"v":1,"handle":h}), same).await.0);
    }
    assert_eq!(codes, [204, 204, 204, 204, 204, 429, 429]);
    assert_eq!(r.wake(&h).await, 204, "another source IP is fine");
}

#[tokio::test]
async fn wake_rate_limited_globally() {
    let (r, s) = plain_with(Rates { wake_global: Limit::hourly(4), ..Rates::default() }).await;
    let h = r.make_handle(&s, "tok:gl").await;
    let mut codes = vec![];
    for _ in 0..6 {
        codes.push(r.wake(&h).await);
    }
    assert_eq!(codes, [204, 204, 204, 204, 429, 429]);
}

#[tokio::test]
async fn replayed_wake_is_capped_by_the_handle_limit() {
    let (r, s) = plain().await;
    let h = r.make_handle(&s, "tok:replay").await;
    // The identical captured request, 60 times, from 60 sources.
    let mut n = 0;
    for _ in 0..60 {
        if r.wake(&h).await == 204 {
            n += 1;
        }
    }
    assert_eq!(n, 10);
    assert_eq!(s.wake_count(), 10);
}

#[tokio::test]
async fn flood_of_distinct_ips_and_addresses_stays_bounded() {
    let cap = 32;
    let (r, s) = plain_with(Rates {
        map_cap: cap,
        wake_global: Limit::hourly(200),
        issue_global: Limit::hourly(150),
        ..Rates::default()
    })
    .await;
    // Garbage handles from 300 distinct IPs: the per-IP map stays at its cap
    // and the global limit still holds as the backstop.
    let mut limited = 0;
    for _ in 0..300 {
        let ip = r.ip();
        let (st, _) = r.post("/v1/wake", &json!({"v":1,"handle":"garbage"}), &ip).await;
        if st == 429 {
            limited += 1;
        }
        assert!(r.state.map_sizes().iter().all(|n| *n <= cap), "{:?}", r.state.map_sizes());
    }
    assert!(limited >= 100, "global limit must hold, got {limited}");
    assert_eq!(s.wake_count(), 0);
    // Registrations for 300 distinct addresses from 300 IPs.
    let mut limited = 0;
    for i in 0..300 {
        let (st, _) = r.register(&format!("tok:flood{i}")).await;
        if st == 429 {
            limited += 1;
        }
        assert!(r.state.map_sizes().iter().all(|n| *n <= cap.max(256)), "{:?}", r.state.map_sizes());
    }
    assert!(limited >= 100, "global issuance limit must hold, got {limited}");
    let sizes = r.state.map_sizes();
    assert!(sizes[..6].iter().all(|n| *n <= cap), "{sizes:?}");
}

#[tokio::test]
async fn concurrent_sends_are_capped() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let (r, s) = plain_with(Rates { max_in_flight: 2, ..Rates::default() }).await;
    let h = r.make_handle(&s, "tok:cc").await;
    *s.gate.lock().unwrap() = Some(gate.clone());
    let r = Arc::new(r);
    // Each task reports its status as it finishes.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    for _ in 0..5 {
        let (r, h, tx) = (r.clone(), h.clone(), tx.clone());
        tokio::spawn(async move {
            let _ = tx.send(r.wake(&h).await);
        });
    }
    // Two requests hold the gate until it opens, so the first three to
    // finish are the refusals. Waiting for them needs no clock.
    let mut codes = vec![];
    for _ in 0..3 {
        codes.push(rx.recv().await.unwrap());
    }
    assert_eq!(codes, [503, 503, 503]);
    gate.add_permits(10);
    for _ in 0..2 {
        codes.push(rx.recv().await.unwrap());
    }
    codes.sort();
    assert_eq!(codes, [204, 204, 503, 503, 503]);
}

#[tokio::test]
async fn xff_is_honoured_only_from_the_trusted_proxy() {
    let mk = |trusted: Option<&str>| {
        let s = Arc::new(FakeSender::default());
        let setup = Setup {
            rates: Rates { wake_ip: Limit::hourly(2), ..Rates::default() },
            trusted_proxy: trusted.map(|t| t.parse().unwrap()),
        };
        (setup, s)
    };
    // Trusted peer (127.0.0.1): every distinct XFF is its own source.
    let (setup, s) = mk(Some("127.0.0.1"));
    let r = start(setup, s.clone()).await;
    let h = r.make_handle(&s, "tok:x1").await;
    for _ in 0..5 {
        assert_eq!(r.wake(&h).await, 204);
    }
    // Untrusted peer: the header is ignored, all share the peer's bucket.
    let (setup, s) = mk(Some("10.9.9.9"));
    let r = start(setup, s.clone()).await;
    let h = {
        let (st, b) = r.post("/v1/handles", &Relay::reg_body("tok:x2"), "1.1.1.1").await;
        assert_eq!(st, 202);
        let (st, b) = r
            .post(
                "/v1/handles/confirm",
                &Relay::confirm_body(b["challenge_id"].as_str().unwrap(), &s.last_code(), "tok:x2"),
                "2.2.2.2",
            )
            .await;
        assert_eq!(st, 200);
        b["handle"].as_str().unwrap().to_string()
    };
    let mut codes = vec![];
    for i in 0..4 {
        codes.push(r.post("/v1/wake", &json!({"v":1,"handle":h}), &format!("9.9.9.{i}")).await.0);
    }
    assert_eq!(codes, [204, 204, 429, 429]);
    // No trusted proxy configured at all: same.
    let (setup, s) = mk(None);
    let r = start(setup, s.clone()).await;
    let (st, b) = r.post("/v1/handles", &Relay::reg_body("tok:x3"), "1.1.1.1").await;
    assert_eq!(st, 202);
    let (_, b) = r
        .post("/v1/handles/confirm", &Relay::confirm_body(b["challenge_id"].as_str().unwrap(), &s.last_code(), "tok:x3"), "2.2.2.2")
        .await;
    let h = b["handle"].as_str().unwrap().to_string();
    let mut codes = vec![];
    for i in 0..3 {
        codes.push(r.post("/v1/wake", &json!({"v":1,"handle":h}), &format!("8.8.8.{i}")).await.0);
    }
    assert_eq!(codes, [204, 204, 429]);
}

// ---- registration ----

#[tokio::test]
async fn challenge_reuse_is_refused() {
    let (r, s) = plain().await;
    let (_, b) = r.register("tok:reuse").await;
    let id = b["challenge_id"].as_str().unwrap().to_string();
    let code = s.last_code();
    let ip = r.ip();
    let body = Relay::confirm_body(&id, &code, "tok:reuse");
    assert_eq!(r.post("/v1/handles/confirm", &body, &ip).await.0, 200);
    let (st, b) = r.post("/v1/handles/confirm", &body, &ip).await;
    assert_eq!((st, b["error"].as_str()), (404, Some("unknown_challenge")));
}

#[tokio::test]
async fn challenge_exhaustion_returns_503_and_keeps_pending() {
    let (r, s) = plain_with(Rates { issue_global: Limit::hourly(10_000), ..Rates::default() }).await;
    let mut first = None;
    for i in 0..256 {
        let (st, b) = r.register(&format!("tok:ex{i}")).await;
        assert_eq!(st, 202, "{i}");
        if i == 0 {
            first = Some((b["challenge_id"].as_str().unwrap().to_string(), s.last_code()));
        }
    }
    let (st, b) = r.register("tok:one-too-many").await;
    assert_eq!((st, b["error"].as_str()), (503, Some("busy")));
    // Nothing was evicted: the oldest pending challenge still confirms.
    let (id, code) = first.unwrap();
    let ip = r.ip();
    let (st, _) = r.post("/v1/handles/confirm", &Relay::confirm_body(&id, &code, "tok:ex0"), &ip).await;
    assert_eq!(st, 200);
    // And its slot is free again.
    assert_eq!(r.register("tok:after").await.0, 202);
}

#[tokio::test]
async fn binding_mismatch_on_confirm_is_refused() {
    let (r, s) = plain().await;
    let (_, b) = r.register("tok:bind").await;
    let id = b["challenge_id"].as_str().unwrap().to_string();
    let code = s.last_code();
    let ip = r.ip();
    let (st, b) = r.post("/v1/handles/confirm", &Relay::confirm_body(&id, &code, "tok:other"), &ip).await;
    assert_eq!((st, b["error"].as_str()), (403, Some("challenge_failed")));
    // A wrong code gives the same answer.
    let (st, b) = r.post("/v1/handles/confirm", &Relay::confirm_body(&id, "wrong", "tok:bind"), &ip).await;
    assert_eq!((st, b["error"].as_str()), (403, Some("challenge_failed")));
    // The third failure burns it.
    assert_eq!(r.post("/v1/handles/confirm", &Relay::confirm_body(&id, "wrong", "tok:bind"), &ip).await.0, 403);
    assert_eq!(r.post("/v1/handles/confirm", &Relay::confirm_body(&id, &code, "tok:bind"), &ip).await.0, 404);
}

#[tokio::test]
async fn unsupported_app_and_platform_are_400() {
    let (r, s) = plain().await;
    let ip = r.ip();
    let mut b = Relay::reg_body("tok:a");
    b["app"] = json!("com.evil.app");
    let (st, e) = r.post("/v1/handles", &b, &ip).await;
    assert_eq!((st, e["error"].as_str()), (400, Some("unsupported_app")));
    let mut b = Relay::reg_body("tok:a");
    b["platform"] = json!("ios");
    b["address_kind"] = json!("apns");
    let (st, e) = r.post("/v1/handles", &b, &ip).await;
    assert_eq!((st, e["error"].as_str()), (400, Some("unsupported_platform")));
    let mut b = Relay::reg_body("tok:a");
    b["platform"] = json!("symbian");
    assert_eq!(r.post("/v1/handles", &b, &ip).await.0, 400);
    // A kind that does not belong to the platform.
    let mut b = Relay::reg_body("tok:a");
    b["address_kind"] = json!("apns");
    assert_eq!(r.post("/v1/handles", &b, &ip).await.0, 400);
    assert!(s.challenges.lock().unwrap().is_empty());
}

#[tokio::test]
async fn address_charset_and_length_are_enforced() {
    let (r, s) = plain().await;
    for bad in ["", "has space", "new\nline", "quo\"te", &"a".repeat(256), "ünï"] {
        let ip = r.ip();
        let (st, _) = r.post("/v1/handles", &Relay::reg_body(bad), &ip).await;
        assert_eq!(st, 400, "{bad:?}");
    }
    assert!(s.challenges.lock().unwrap().is_empty());
    let (st, _) = r.register(&"a".repeat(255)).await;
    assert_eq!(st, 202);
}

// ---- misc ----

#[tokio::test]
async fn oversized_body_is_413() {
    let (r, _s) = plain().await;
    let ip = r.ip();
    let big = vec![b'{'; 5000];
    let (st, b) = r.post_raw("/v1/wake", big, &ip).await;
    assert_eq!((st, b["error"].as_str()), (413, Some("too_large")));
}

#[tokio::test]
async fn unknown_route_404_and_healthz() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (r, _s) = plain().await;
    async fn get(addr: std::net::SocketAddr, path: &str) -> String {
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        c.write_all(format!("GET {path} HTTP/1.0\r\n\r\n").as_bytes()).await.unwrap();
        let mut out = String::new();
        c.read_to_string(&mut out).await.unwrap();
        out
    }
    let ok = get(r.addr, "/healthz").await;
    assert!(ok.starts_with("HTTP/1.0 200") || ok.starts_with("HTTP/1.1 200"), "{ok}");
    assert!(ok.ends_with(r#"{"ok":true}"#));
    let nf = get(r.addr, "/nope").await;
    assert!(nf.contains(" 404 ") && nf.ends_with(r#"{"error":"not_found"}"#), "{nf}");
}
