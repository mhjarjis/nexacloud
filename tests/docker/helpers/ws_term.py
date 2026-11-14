#!/usr/bin/env python3
"""Minimal WebSocket client for the NexaCloud terminal (stdlib only).
Usage: ws_term.py BASE_URL COOKIE_JAR QUERY COMMAND EXPECT [ORIGIN]
Exits 0 when EXPECT appears in the output within 20 s; prints the HTTP status
line on a refused handshake and exits 2."""
import base64, os, socket, ssl, struct, sys, time
from urllib.parse import urlparse

base, jar, query, command, expect = sys.argv[1:6]
origin = sys.argv[6] if len(sys.argv) > 6 else base
u = urlparse(base)
cookies = []
for line in open(jar):
    line = line.strip()
    if line.startswith("#HttpOnly_"):
        line = line[len("#HttpOnly_"):]
    if not line or line.startswith("#"):
        continue
    f = line.split("\t")
    if len(f) >= 7:
        cookies.append(f"{f[5]}={f[6]}")
raw = socket.create_connection((u.hostname, u.port or 443), timeout=20)
ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE
s = ctx.wrap_socket(raw, server_hostname=u.hostname)
key = base64.b64encode(os.urandom(16)).decode()
req = (f"GET /api/terminal?{query} HTTP/1.1\r\nHost: {u.netloc}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
       f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nOrigin: {origin}\r\nCookie: {'; '.join(cookies)}\r\n\r\n")
s.sendall(req.encode())
head = b""
while b"\r\n\r\n" not in head:
    c = s.recv(1)
    if not c:
        break
    head += c
status = head.split(b"\r\n", 1)[0].decode(errors="replace")
if " 101 " not in status:
    print(status); sys.exit(2)

def send(payload: bytes, op=2):
    mask = os.urandom(4)
    n = len(payload)
    hdr = bytes([0x80 | op]) + (bytes([0x80 | n]) if n < 126 else bytes([0x80 | 126]) + struct.pack(">H", n))
    s.sendall(hdr + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))

def recv_exact(n):
    b = b""
    while len(b) < n:
        c = s.recv(n - len(b))
        if not c:
            raise EOFError
        b += c
    return b

out = b""
deadline = time.time() + 20
time.sleep(0.5)
send(command.encode() + b"\n")
s.settimeout(2)
while time.time() < deadline:
    try:
        h = recv_exact(2)
    except (socket.timeout, ssl.SSLError):
        continue
    except EOFError:
        break
    n = h[1] & 0x7F
    if n == 126:
        n = struct.unpack(">H", recv_exact(2))[0]
    elif n == 127:
        n = struct.unpack(">Q", recv_exact(8))[0]
    out += recv_exact(n)
    if expect.encode() in out:
        send(b"exit\n")
        print("ok")
        sys.exit(0)
sys.stdout.write(out.decode(errors="replace")[-2000:])
sys.exit(1)
