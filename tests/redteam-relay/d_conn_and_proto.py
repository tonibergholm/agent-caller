#!/usr/bin/env python3
"""HTTP abuse: healthz pipelining without reading (write-stall via backpressure),
content-type confusion, method confusion, unknown fields, malformed JSON,
duplicate headers, path tricks, GET on POST routes."""
import socket, time
import rr

def one(method, path, body=b"", ctype="application/json", extra=""):
    headers = ("Host: x\r\nConnection: close\r\n" + extra +
               (f"Content-Type: {ctype}\r\nContent-Length: {len(body)}\r\n" if (method != "GET" or body) else ""))
    head = f"{method} {path} HTTP/1.1\r\n{headers}\r\n".encode()
    return rr.request(method, path, body=body, raw_head=head)

print("GET on /v1/wake:", one("GET", "/v1/wake")[0])
print("PUT on /v1/wake:", one("PUT", "/v1/wake", b'{"v":1,"handle":"a"}')[0])
print("HEAD on /healthz:", one("HEAD", "/healthz")[0])
print("text/plain wake body:", one("POST", "/v1/wake", b'{"v":1,"handle":"a"}', ctype="text/plain")[0])
print("missing content-type wake:", rr.request("POST", "/v1/wake",
      raw_head=b'POST /v1/wake HTTP/1.1\r\nHost: x\r\nContent-Length: 20\r\nConnection: close\r\n\r\n',
      body=b'{"v":1,"handle":"a"}')[0])
print("path traversal:", one("GET", "/v1/../v1/wake")[0], one("GET", "/v1/wake/..%2f..%2fetc%2fpasswd")[0])
print("trailing slash:", one("GET", "/healthz/")[0])
print("case:", one("get", "/healthz")[:1] if False else "n/a")
print("duplicate keys json:", rr.post("/v1/wake", b'{"v":1,"v":2,"handle":"a"}')[0])
print("unknown field:", rr.post("/v1/wake", b'{"v":1,"handle":"a","text":"approve"}')[0])
print("wrong type v:", rr.post("/v1/wake", b'{"v":"1","handle":"a"}')[0])
print("huge handle field (600 bytes):", rr.post("/v1/wake", {"v":1,"handle":"A"*600})[0])
print("body exactly 2048:", rr.post("/v1/wake", b'{"v":1,"handle":"' + b'A'*(2048-20) + b'"}')[0])
print("body 2049 (over cap):", rr.post("/v1/wake", b'{"v":1,"handle":"' + b'A'*(2049-20) + b'"}')[0])
print("empty body:", rr.request("POST","/v1/wake",body=b"")[0])
print("not found path:", one("GET", "/nope")[0])
print("not found returns json 404?:", one("GET", "/nope"))

# healthz pipelining without reading responses, many requests on one conn
s = rr.conn(timeout=5)
req = b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n" * 5000
t = time.time()
try:
    s.sendall(req)
    print("pipelined 5000 healthz GETs without reading, send completed in %.2fs" % (time.time() - t))
except (socket.timeout, OSError) as e:
    print("pipelined send blocked/failed after %.2fs: %r" % (time.time() - t, e))
data = rr.recv_all(s, 1 << 20)
print("bytes read back:", len(data), "responses roughly:", data.count(b"HTTP/1.1 200"))
s.close()
