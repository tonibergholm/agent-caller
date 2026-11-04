#!/usr/bin/env python3
"""
TOCTOU, idempotency and cross-client abuse against a real open request.

Creates one real `PermissionRequest` by POSTing a recorded Claude Code
hook payload (`tests/fixtures/hooks/claude/payload-PermissionRequest-Bash.json`)
to the hook endpoint over the hook Unix socket. That POST blocks (the hook
holds) until the request is answered, which is exactly what we want: a
live, answerable request with a known `request_id` we can attack from one
or more WebSocket clients while the hook connection is still open.

Cases:
  A. Two WS clients race to answer the same request_id; the loser must
     see `request_gone`, never a second delivery.
  B. Replay the same client-generated command id with the SAME payload
     while the first run is in flight -- must not double-deliver; both
     calls get the same result.
  C. Replay the same command id with a DIFFERENT payload after the first
     finished -- must be `duplicate_mismatch`, not a second delivery.
  D. Answer with a wrong/huge `revision` -- must not deliver blindly.
  E. Answer an already-resolved request_id again -- must be `request_gone`.
  F. A second, unrelated authenticated client (different client.id) can
     answer a request it did not open -- confirmed by design (any token
     holder may act on any request); recorded as expected, not a finding.

Usage:
    python3 -I toctou_idempotency.py --dir <acd config dir> --port <ws port>

Safety: connects only to 127.0.0.1 and the hook Unix socket under <dir>.
"""
import argparse
import json
import os
import socket
import sys
import threading
import time
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import acdws  # noqa: E402

REPO_ROOT = os.path.normpath(os.path.join(os.path.dirname(__file__), "..", ".."))
FIXTURE = os.path.join(
    REPO_ROOT, "tests", "fixtures", "hooks", "claude",
    "payload-PermissionRequest-Bash.json",
)


def post_hook_blocking(hook_sock_path: str, hook_token: str, payload: dict):
    """Opens the hook connection and sends the POST, but does not read the
    response (the request stays open/pending until this returns a socket
    the caller can read from later, once the request is answered)."""
    body = json.dumps(payload).encode()
    req = (
        f"POST /hook/claude/PermissionRequest HTTP/1.1\r\n"
        f"Host: 127.0.0.1\r\nContent-Type: application/json\r\n"
        f"Content-Length: {len(body)}\r\nConnection: close\r\n"
        f"X-Acd-Token: {hook_token}\r\n\r\n"
    ).encode() + body
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(30)
    s.connect(hook_sock_path)
    s.sendall(req)
    return s


def read_http_response(sock, timeout=30):
    sock.settimeout(timeout)
    buf = b""
    while True:
        chunk = sock.recv(65536)
        if not chunk:
            break
        buf += chunk
    return buf


def connect_authed(port, token, client_id=None):
    ws = acdws.WS.connect("127.0.0.1", port, use_tls=True)
    ws.send_json(acdws.hello_msg(token, client_id=client_id))
    ws.recv_json(timeout=5)
    ws.recv_json(timeout=5)
    return ws


def wait_for_request_opened(ws, timeout=10):
    deadline = time.time() + timeout
    while time.time() < deadline:
        msg = ws.recv_json(timeout=max(0.1, deadline - time.time()))
        if msg and msg.get("type") == "request.opened":
            return msg["request"]
    raise TimeoutError("no request.opened within timeout")


def recv_result_for(ws, cmd_id, timeout=10):
    """Reads frames until the `result` matching `cmd_id`, skipping any
    `agent.updated`/`request.*` broadcast events interleaved on the same
    connection (every acd/1 reply and event share one stream)."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        msg = ws.recv_json(timeout=max(0.1, deadline - time.time()))
        if msg is None:
            continue
        if msg.get("type") == "result" and msg.get("id") == cmd_id:
            return msg
    raise TimeoutError(f"no result for {cmd_id} within timeout")


def answer(ws, request_id, option_id=None, revision=None, cmd_id=None):
    cmd_id = cmd_id or acdws.new_id()
    msg = {
        "id": cmd_id,
        "type": "request.answer",
        "request_id": request_id,
        "option_id": option_id,
    }
    if revision is not None:
        msg["revision"] = revision
    ws.send_json(msg)
    return recv_result_for(ws, cmd_id, timeout=10)


def fresh_hook_request(hook_sock, hook_token, unique_tag):
    payload = json.load(open(FIXTURE))
    payload["session_id"] = str(uuid.uuid4())
    payload["prompt_id"] = str(uuid.uuid4())
    payload["tool_input"]["command"] = f"touch /tmp/redteam-{unique_tag}.marker"
    return post_hook_blocking(hook_sock, hook_token, payload)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--port", type=int, required=True)
    args = ap.parse_args()
    token = open(os.path.join(args.dir, "client.token")).read().strip()
    hook_token = json.load(open(os.path.join(args.dir, "hook-tokens.json")))["current"]
    hook_sock = os.path.join(args.dir, "hook.sock")

    results = []

    def record(name, outcome, detail=""):
        results.append((name, outcome, detail))
        print(f"[{outcome:>16}] {name}  {detail}")

    # --- Case A: two clients race to answer the same request ---
    ws1 = connect_authed(args.port, token)
    ws2 = connect_authed(args.port, token)
    hook_sock_a = fresh_hook_request(hook_sock, hook_token, "race")
    req1 = wait_for_request_opened(ws1)
    req2 = wait_for_request_opened(ws2)
    assert req1["id"] == req2["id"], "both clients must see the same request"
    rid = req1["id"]

    done = {}

    def do_answer(ws, key):
        done[key] = answer(ws, rid, option_id="allow")

    t1 = threading.Thread(target=do_answer, args=(ws1, "a"))
    t2 = threading.Thread(target=do_answer, args=(ws2, "b"))
    t1.start()
    t2.start()
    t1.join()
    t2.join()
    oks = [k for k, v in done.items() if v.get("ok")]
    errs = [k for k, v in done.items() if not v.get("ok")]
    record(
        "race_two_clients_same_request",
        "ok" if len(oks) == 1 and len(errs) == 1 else "UNEXPECTED",
        f"winner={oks} loser_error={[done[k]['error']['code'] for k in errs]}",
    )
    hook_resp = read_http_response(hook_sock_a, timeout=10)
    record("race_hook_reply", "info", hook_resp.split(b"\r\n")[0])
    ws1.close()
    ws2.close()

    # --- Case B: replay same id with same payload while in flight ---
    # (We approximate "in flight" by firing two identical commands back to
    # back without waiting; the server's reservation should make the
    # second one either wait for the first result or return the same
    # cached result -- never a second delivery.)
    ws = connect_authed(args.port, token)
    hook_sock_b = fresh_hook_request(hook_sock, hook_token, "replay-same")
    req = wait_for_request_opened(ws)
    cmd_id = acdws.new_id()
    msg = {
        "id": cmd_id, "type": "request.answer",
        "request_id": req["id"], "option_id": "allow",
    }
    ws.send_json(msg)
    ws.send_json(msg)  # identical replay, same id+payload
    r1 = recv_result_for(ws, cmd_id, timeout=10)
    # Both sends share one `id`; acd must not emit two different results
    # for it. Read once more briefly to see whether a second, distinct
    # result frame for the same id ever arrives (it must not).
    r2 = r1
    try:
        extra = ws.recv_json(timeout=1.5)
        if extra and extra.get("type") == "result" and extra.get("id") == cmd_id:
            r2 = extra
    except (socket.timeout, TimeoutError, ConnectionError):
        pass
    same = (r1 == r2) or (r1.get("ok") and r2.get("ok"))
    record("replay_same_id_same_payload", "ok" if same else "UNEXPECTED", f"{r1} / {r2}")
    read_http_response(hook_sock_b, timeout=10)
    ws.close()

    # --- Case C: replay same id with a DIFFERENT payload after the first
    # finished ---
    ws = connect_authed(args.port, token)
    hook_sock_c = fresh_hook_request(hook_sock, hook_token, "replay-diff")
    req = wait_for_request_opened(ws)
    cmd_id = acdws.new_id()
    r1 = answer(ws, req["id"], option_id="allow", cmd_id=cmd_id)
    read_http_response(hook_sock_c, timeout=10)
    # second call, same id, different option -- after the first is resolved
    msg2 = {"id": cmd_id, "type": "request.answer", "request_id": req["id"], "option_id": "deny"}
    ws.send_json(msg2)
    r2 = recv_result_for(ws, cmd_id, timeout=10)
    code = r2.get("error", {}).get("code") if not r2.get("ok") else None
    record(
        "replay_same_id_diff_payload_after_finish",
        "ok" if code == "duplicate_mismatch" else "UNEXPECTED",
        str(r2),
    )
    ws.close()

    # --- Case D: huge / wrong revision ---
    ws = connect_authed(args.port, token)
    hook_sock_d = fresh_hook_request(hook_sock, hook_token, "revision")
    req = wait_for_request_opened(ws)
    r = answer(ws, req["id"], option_id="allow", revision=2**62)
    code = r.get("error", {}).get("code") if not r.get("ok") else None
    record(
        "answer_huge_revision",
        "ok" if code in ("request_changed", "bad_request") else "UNEXPECTED",
        str(r),
    )
    # The bad-revision answer must not have delivered anything, so the
    # hook is still held; answer again with the correct (omitted)
    # revision to clean it up and confirm it was genuinely not delivered.
    r_fix = answer(ws, req["id"], option_id="allow", cmd_id=acdws.new_id())
    record("answer_huge_revision_then_correct", "info", str(r_fix))
    hook_resp = read_http_response(hook_sock_d, timeout=10)
    record("answer_huge_revision_hook_reply", "info", hook_resp.split(b"\r\n")[0])
    ws.close()

    # --- Case E: answer an already-resolved request again ---
    ws = connect_authed(args.port, token)
    hook_sock_e = fresh_hook_request(hook_sock, hook_token, "resolved-twice")
    req = wait_for_request_opened(ws)
    answer(ws, req["id"], option_id="allow")
    read_http_response(hook_sock_e, timeout=10)
    r2 = answer(ws, req["id"], option_id="allow", cmd_id=acdws.new_id())
    code = r2.get("error", {}).get("code") if not r2.get("ok") else None
    record(
        "answer_already_resolved_request",
        "ok" if code == "request_gone" else "UNEXPECTED",
        str(r2),
    )
    ws.close()

    # --- Case F: a different client.id answers a request it did not open ---
    ws_owner = connect_authed(args.port, token)
    ws_other = connect_authed(args.port, token, client_id=acdws.new_id())
    hook_sock_f = fresh_hook_request(hook_sock, hook_token, "cross-client")
    req = wait_for_request_opened(ws_owner)
    wait_for_request_opened(ws_other)  # other client also sees it broadcast
    r = answer(ws_other, req["id"], option_id="deny")
    record(
        "cross_client_answers_unrelated_request",
        "by-design-any-token-holder" if r.get("ok") else "refused",
        str(r),
    )
    read_http_response(hook_sock_f, timeout=10)
    ws_owner.close()
    ws_other.close()

    print("\n--- summary ---")
    for name, outcome, detail in results:
        print(f"{outcome:>16}  {name}")


if __name__ == "__main__":
    main()
