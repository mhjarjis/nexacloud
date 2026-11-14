#!/usr/bin/env python3
"""Fake Telegram Bot API (HTTP :8099), SMTP server (:2525) and DNS APIs
(Cloudflare under /cf, Hostinger under /hs on HTTP :8098) for e2e tests.
Every request / message is appended to /tmp/fake-telegram.log and /tmp/fake-smtp.log."""
import json, socketserver, threading, uuid
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


GOOD = 'Bearer good-dns-token-0123456789abcdef'
CF = {'z1': []}            # Cloudflare zone "example.test" -> records
HS = {'example.test': []}  # Hostinger zone -> record sets

class Dns(BaseHTTPRequestHandler):
    def reply(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code); self.send_header('Content-Type', 'application/json'); self.send_header('Content-Length', str(len(b))); self.end_headers(); self.wfile.write(b)
    def body(self):
        n = int(self.headers.get('Content-Length', 0) or 0)
        return json.loads(self.rfile.read(n) or b'{}') if n else {}
    def handle_any(self, m):
        if self.headers.get('Authorization') != GOOD:
            return self.reply(401, {'success': False, 'errors': [{'message': 'Invalid API token'}], 'message': 'Unauthenticated'})
        p = self.path.split('?')[0]
        q = self.path.split('?')[1] if '?' in self.path else ''
        if p.startswith('/cf'):
            p = p[3:]
            if p == '/user/tokens/verify': return self.reply(200, {'success': True, 'result': {'status': 'active'}})
            if p == '/zones': return self.reply(200, {'success': True, 'result': [{'id': 'z1', 'name': 'example.test'}] if 'name=example.test' in q else []})
            if p == '/zones/z1/dns_records':
                if m == 'GET': return self.reply(200, {'success': True, 'result': CF['z1']})
                r = self.body(); r['id'] = uuid.uuid4().hex[:8]; CF['z1'].append(r); return self.reply(200, {'success': True, 'result': r})
            if p.startswith('/zones/z1/dns_records/'):
                rid = p.rsplit('/', 1)[1]
                if m == 'DELETE': CF['z1'][:] = [x for x in CF['z1'] if x['id'] != rid]; return self.reply(200, {'success': True})
                r = self.body(); r['id'] = rid; CF['z1'][:] = [r if x['id'] == rid else x for x in CF['z1']]; return self.reply(200, {'success': True, 'result': r})
            return self.reply(404, {'success': False, 'errors': [{'message': 'not found'}]})
        if p.startswith('/hs'):
            p = p[3:]
            if p == '/api/domains/v1/portfolio': return self.reply(200, [{'domain': 'example.test'}])
            if p == '/api/dns/v1/zones/example.test':
                z = HS['example.test']
                if m == 'GET': return self.reply(200, z)
                b = self.body()
                if m == 'PUT':
                    for s in b.get('zone', []):
                        z[:] = [x for x in z if not (x['name'] == s['name'] and x['type'] == s['type'])] if b.get('overwrite') else z
                        z.append(s)
                    return self.reply(200, {'message': 'ok'})
                if m == 'DELETE':
                    for f in b.get('filters', []):
                        z[:] = [x for x in z if not (x['name'] == f['name'] and x['type'] == f['type'])]
                    return self.reply(200, {'message': 'ok'})
            return self.reply(404, {'message': 'not found'})
        self.reply(404, {})
    def do_GET(self): self.handle_any('GET')
    def do_POST(self): self.handle_any('POST')
    def do_PUT(self): self.handle_any('PUT')
    def do_DELETE(self): self.handle_any('DELETE')
    def log_message(self, *a): pass

socketserver.ThreadingTCPServer.allow_reuse_address = True
threading.Thread(target=HTTPServer(('127.0.0.1', 8098), Dns).serve_forever, daemon=True).start()
threading.Thread(target=socketserver.ThreadingTCPServer(('127.0.0.1', 2525), Smtp).serve_forever, daemon=True).start()
HTTPServer(('127.0.0.1', 8099), Tg).serve_forever()
