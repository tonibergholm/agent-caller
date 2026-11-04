//! What the relay does with bytes from Google and with addresses from
//! devices (acd-relay/src/fcm.rs).
//! Invariants: `map_fcm_error` never panics on any body, depends on the body
//! alone (not the status), gives the same class for the body and for any
//! re-serialisation of it, and says `Gone` only when the parsed body has
//! `UNREGISTERED` as a `status` or an `errorCode` value; the wake and
//! challenge bodies built for any address and code have exactly the fixed
//! keys (nothing a device sends adds a field), the address and code come
//! back unchanged out of the JSON, and the wake body holds the fixed text and
//! nothing else; `Debug` of an address shows its length, not its value.
#![no_main]
use acd_relay::fcm::{Address, SendError, challenge_body, map_fcm_error, wake_body};
use acd_relay::wire::AddressKind;
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

#[derive(Arbitrary, Debug)]
struct Input {
    status: u16,
    other_status: u16,
    kind: u8,
    value: String,
    code: String,
    body: Vec<u8>,
}

/// Does some object in the tree hold `UNREGISTERED` under `status` or `errorCode`?
fn mentions(v: &Value) -> bool {
    match v {
        Value::Object(m) => {
            m.iter().any(|(k, x)| (k == "status" || k == "errorCode") && x == "UNREGISTERED") || m.values().any(mentions)
        }
        Value::Array(a) => a.iter().any(mentions),
        _ => false,
    }
}

fn keys(v: &Value) -> Vec<&str> {
    let mut k: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    k.sort();
    k
}

fuzz_target!(|i: Input| {
    let class = map_fcm_error(i.status, &i.body);
    assert_eq!(class, map_fcm_error(i.other_status, &i.body), "the class depends on the status");
    if class == SendError::Gone {
        let v: Value = serde_json::from_slice(&i.body).expect("Gone needs a json body");
        assert!(mentions(&v), "Gone without UNREGISTERED in a status or errorCode");
    }
    if let Ok(v) = serde_json::from_slice::<Value>(&i.body) {
        for again in [serde_json::to_vec(&v).unwrap(), serde_json::to_vec_pretty(&v).unwrap()] {
            assert_eq!(map_fcm_error(i.status, &again), class, "a re-serialisation changes the class");
        }
        if mentions(&v) && v["error"]["status"] == "UNREGISTERED" {
            assert_eq!(class, SendError::Gone);
        }
    }

    let kind = match i.kind % 3 {
        0 => AddressKind::FcmToken,
        1 => AddressKind::FcmFid,
        _ => AddressKind::Apns,
    };
    let to = Address { kind, value: i.value.clone() };
    let dbg = format!("{to:?}");
    assert_eq!(dbg, format!("Address({kind:?}, len {})", i.value.len()), "Debug shows the address");
    let w = wake_body(&to);
    let c = challenge_body(&to, &i.code);
    for (b, expect_data) in [(&w, vec!["v"]), (&c, vec!["c", "t", "v"])] {
        assert_eq!(keys(b), ["message"]);
        let m = &b["message"];
        let target = if kind == AddressKind::FcmFid { "fid" } else { "token" };
        let mut want = vec![target, "data", "androidV2"];
        want.sort();
        assert_eq!(keys(m), want);
        assert_eq!(m[target], Value::String(i.value.clone()));
        assert_eq!(keys(&m["data"]), expect_data);
        // The same text comes back after the body goes over the wire.
        let back: Value = serde_json::from_str(&b.to_string()).unwrap();
        assert_eq!(&back, b);
    }
    assert_eq!(c["message"]["data"]["c"], Value::String(i.code.clone()));
    let text = w.to_string();
    assert!(!text.contains("approve") && !text.contains("permission"), "free text in the wake");
});
