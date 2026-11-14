#!/usr/bin/env python3
"""Print the current TOTP code for a base32 secret (RFC 6238, SHA-1, 6 digits, 30 s).
Usage: totp.py SECRET [offset_steps]"""
import base64, hmac, hashlib, struct, sys, time
s = sys.argv[1].strip().upper().replace(" ", "")
s += "=" * (-len(s) % 8)
key = base64.b32decode(s)
step = int(time.time()) // 30 + (int(sys.argv[2]) if len(sys.argv) > 2 else 0)
h = hmac.new(key, struct.pack(">Q", step), hashlib.sha1).digest()
o = h[-1] & 15
print("%06d" % ((struct.unpack(">I", h[o:o + 4])[0] & 0x7FFFFFFF) % 1000000))
