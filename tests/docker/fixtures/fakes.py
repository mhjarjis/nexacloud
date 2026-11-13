#!/usr/bin/env python3
"""Fake Telegram Bot API (HTTP :8099) and SMTP server (:2525) for e2e tests.
Every request / message is appended to /tmp/fake-telegram.log and /tmp/fake-smtp.log."""
import socketserver, threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs

class Tg(BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get('Content-Length', 0))).decode()
        q = parse_qs(body)
        with open('/tmp/fake-telegram.log', 'a') as f:
            f.write(f"{self.path} chat={q.get('chat_id', [''])[0]} text={q.get('text', [''])[0]!r}\n")
        ok = '/bot123:good/' in self.path
        self.send_response(200 if ok else 401)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(b'{"ok":true}' if ok else b'{"ok":false,"description":"Unauthorized"}')
    def log_message(self, *a): pass

class Smtp(socketserver.StreamRequestHandler):
    def handle(self):
        w = lambda s: self.wfile.write((s + '\r\n').encode())
        w('220 fake ESMTP')
        data, in_data = [], False
        for raw in self.rfile:
            line = raw.decode(errors='replace').rstrip('\r\n')
            if in_data:
                if line == '.':
                    in_data = False
                    with open('/tmp/fake-smtp.log', 'a') as f:
                        f.write('\n'.join(data) + '\n----\n')
                    data = []
                    w('250 queued')
                else:
                    data.append(line)
                continue
            cmd = line[:4].upper()
            if cmd in ('EHLO', 'HELO'): w('250 fake')
            elif cmd == 'DATA': in_data = True; w('354 go')
            elif cmd == 'QUIT': w('221 bye'); break
            else: w('250 ok')

socketserver.ThreadingTCPServer.allow_reuse_address = True
threading.Thread(target=socketserver.ThreadingTCPServer(('127.0.0.1', 2525), Smtp).serve_forever, daemon=True).start()
HTTPServer(('127.0.0.1', 8099), Tg).serve_forever()
