"""
Minimal, hand-rolled WebSocket client for attacking a local acd/1 server.

Deliberately not using `websockets` or `tungstenite`-equivalent libraries:
the point of this harness is to send frames a conforming client would
never produce (oversized, malformed, fragmented, control-opcode floods),
so it speaks RFC 6455 framing directly over a raw TLS or TCP socket.

Stdlib only. Safety: `connect()` refuses any host that is not
127.0.0.0/8, ::1, or a local Unix socket path, so these helpers cannot be
pointed at a real daemon by accident.

Run with `python3 -I` (isolated mode) since this directory can contain
other scripts; `-I` ignores PYTHONPATH and the script's own directory for
third-party-module shadowing, which does not matter here (stdlib only)
but costs nothing to keep.
"""
from __future__ import annotations

import ipaddress
import json
import os
import socket
import ssl
import struct
import threading
import time
import uuid
from dataclasses import dataclass, field
from typing import Optional

OPCODE_CONT = 0x0
OPCODE_TEXT = 0x1
OPCODE_BINARY = 0x2
OPCODE_CLOSE = 0x8
OPCODE_PING = 0x9
OPCODE_PONG = 0xA


def _assert_local(host: str) -> None:
    if host in ("localhost",):
        return
    try:
        ip = ipaddress.ip_address(host)
    except ValueError as e:
        raise ValueError(
            f"refusing non-local host {host!r}: this harness only attacks "
            "127.0.0.0/8 or ::1"
        ) from e
    if not (ip.is_loopback):
        raise ValueError(f"refusing non-loopback host {host!r}")


def tcp_connect(host: str, port: int, timeout: float = 5.0) -> socket.socket:
    _assert_local(host)
    s = socket.create_connection((host, port), timeout=timeout)
    return s


def tls_wrap(sock: socket.socket, server_hostname: Optional[str] = None) -> ssl.SSLSocket:
    """Wraps with certificate verification fully disabled: an attacker does
    not have the pin, and part of what we are testing is whether the server
    requires anything besides a successful TLS handshake before `hello`."""
    ctx = ssl._create_unverified_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx.wrap_socket(sock, server_hostname=server_hostname or "localhost")


def ws_handshake_request(host: str, port: int, path: str = "/", origin: Optional[str] = None) -> bytes:
    key = "dGhlIHNhbXBsZSBub25jZQ=="  # fixed nonce; we never validate accept
    lines = [
        f"GET {path} HTTP/1.1",
        f"Host: {host}:{port}",
        "Upgrade: websocket",
        "Connection: Upgrade",
        f"Sec-WebSocket-Key: {key}",
        "Sec-WebSocket-Version: 13",
    ]
    if origin is not None:
        lines.append(f"Origin: {origin}")
    lines.append("")
    lines.append("")
    return "\r\n".join(lines).encode()


def read_http_response_headers(sock, timeout: float = 5.0) -> bytes:
    sock.settimeout(timeout)
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = sock.recv(4096)
        if not chunk:
            break
        buf += chunk
        if len(buf) > 65536:
            break
    return buf


def mask(data: bytes, key: bytes) -> bytes:
    out = bytearray(len(data))
    for i, b in enumerate(data):
        out[i] = b ^ key[i % 4]
    return bytes(out)


def build_frame(opcode: int, payload: bytes, fin: bool = True, masked: bool = True,
                 rsv: int = 0) -> bytes:
    b0 = (0x80 if fin else 0) | (rsv << 4) | (opcode & 0x0F)
    length = len(payload)
    out = bytearray([b0])
    mbit = 0x80 if masked else 0x00
    if length < 126:
        out.append(mbit | length)
    elif length < 65536:
        out.append(mbit | 126)
        out += struct.pack("!H", length)
    else:
        out.append(mbit | 127)
        out += struct.pack("!Q", length)
    if masked:
        key = os.urandom(4)
        out += key
        out += mask(payload, key)
    else:
        out += payload
    return bytes(out)


def build_raw_header(opcode: int, length_byte_override: int, fin: bool = True) -> bytes:
    """For malformed-frame tests: write the raw 2-byte header without
    producing the matching payload length, so the peer must deal with a
    header that promises more (or less) than what follows."""
    b0 = (0x80 if fin else 0) | (opcode & 0x0F)
    return bytes([b0, length_byte_override])


@dataclass
class Frame:
    opcode: int
    payload: bytes
    fin: bool


def recv_frame(sock, timeout: float = 5.0) -> Optional[Frame]:
    sock.settimeout(timeout)

    def recvn(n):
        buf = b""
        while len(buf) < n:
            chunk = sock.recv(n - len(buf))
            if not chunk:
                raise ConnectionError("peer closed")
            buf += chunk
        return buf

    hdr = recvn(2)
    b0, b1 = hdr[0], hdr[1]
    fin = bool(b0 & 0x80)
    opcode = b0 & 0x0F
    masked = bool(b1 & 0x80)
    length = b1 & 0x7F
    if length == 126:
        length = struct.unpack("!H", recvn(2))[0]
    elif length == 127:
        length = struct.unpack("!Q", recvn(8))[0]
    key = recvn(4) if masked else None
    payload = recvn(length) if length else b""
    if masked:
        payload = mask(payload, key)
    return Frame(opcode=opcode, payload=payload, fin=fin)


class WS:
    """A thin, deliberately low-level WebSocket wrapper around a raw
    socket. Does not hide protocol violations from the caller -- the
    caller constructs frames directly when the test needs that."""

    def __init__(self, sock):
        self.sock = sock

    @classmethod
    def connect(cls, host: str, port: int, use_tls: bool, path: str = "/",
                origin: Optional[str] = None, timeout: float = 5.0) -> "WS":
        _assert_local(host)
        raw = tcp_connect(host, port, timeout=timeout)
        s = tls_wrap(raw) if use_tls else raw
        s.sendall(ws_handshake_request(host, port, path, origin))
        headers = read_http_response_headers(s, timeout=timeout)
        if not headers.startswith(b"HTTP/1.1 101"):
            s.close()
            raise ConnectionRefusedError(headers.decode(errors="replace"))
        return cls(s)

    def send_text(self, text: str):
        self.sock.sendall(build_frame(OPCODE_TEXT, text.encode()))

    def send_json(self, obj):
        self.send_text(json.dumps(obj))

    def send_binary(self, data: bytes):
        self.sock.sendall(build_frame(OPCODE_BINARY, data))

    def send_ping(self, data: bytes = b""):
        self.sock.sendall(build_frame(OPCODE_PING, data))

    def send_raw(self, data: bytes):
        self.sock.sendall(data)

    def recv(self, timeout: float = 5.0) -> Frame:
        return recv_frame(self.sock, timeout=timeout)

    def recv_json(self, timeout: float = 5.0):
        f = self.recv(timeout=timeout)
        if f.opcode == OPCODE_TEXT:
            return json.loads(f.payload.decode())
        return None

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


def new_id() -> str:
    return str(uuid.uuid4())


def hello_msg(token: str, client_id: str = None, name: str = "redteam",
              version: str = "0.0.0-redteam", resume=None, protocol="acd/1"):
    msg = {
        "id": new_id(),
        "type": "hello",
        "protocol": protocol,
        "token": token,
        "client": {"id": client_id or new_id(), "name": name, "version": version},
    }
    if resume is not None:
        msg["resume"] = resume
    return msg
