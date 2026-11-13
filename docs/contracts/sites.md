# Sites, domains, SSL and nginx (Phase 1)

Shared conventions (classes C, J, D and S, Preview, locks, sudo, errors) are defined
in [README.md](README.md). This file covers:
- the site model
- import and adopt
- creating sites, subdomains, redirects and proxies
- DNS checks
- Let's Encrypt
- nginx templates, visual options and the raw editor
- drift detection
- the panel domain

**Sudo baseline.** Every class C or D `apply` needs sudo (README §1.9, following the
Phase 0 precedent). On preview/apply rows, the `Sudo` column marks whether the
**preview** also needs sudo. On J and S rows, it marks the endpoint itself.

## 1. Site model

SQLite table `sites`. The JSON returned by `GET /api/sites/{id}` is a superset of the
v0.1 `GET /api/sites` items, so v0.1 fields keep their names.
```json
{
  "id": "shop.example.com",
  "slug": "shop",
  "managed": true,
  "state": "managed|unmanaged|detached|adopting|drifted|error",
  "kind": "laravel|php|static|proxy|redirect|unknown",
  "runtime": "fpm|frankenphp|none",
  "php_version": "8.3",
  "primary_domain": "shop.example.com",
  "server_names": ["shop.example.com", "www.shop.example.com"],
  "aliases": ["shop.example.net"],
  "redirects": [{"from": "www.shop.example.com", "to": "https://shop.example.com", "code": 301, "preserve_path": true}],
  "user": "nc_shop", "group": "nc_shop",
  "deploy_mode": "releases|in_place|none",
  "paths": {
    "base": "/var/www/shop.example.com",
    "app_root": "/var/www/shop.example.com/current",
    "web_root": "/var/www/shop.example.com/current/public",
    "shared": "/var/www/shop.example.com/shared",
    "nginx_conf": "/etc/nginx/sites-available/shop.example.com.conf",
    "nginx_enabled": "/etc/nginx/sites-enabled/shop.example.com.conf",
    "custom_conf": "/etc/nginx/nexacloud/sites/shop.example.com/custom.conf",
    "pool_conf": "/etc/php/8.3/fpm/pool.d/shop.conf",
    "socket": "/run/php/php8.3-fpm-shop.sock",
    "access_log": "/var/log/nginx/shop.example.com.access.log",
    "error_log": "/var/log/nginx/shop.example.com.error.log"
  },
  "ssl": {
    "enabled": true, "issuer": "letsencrypt|custom|none",
    "status": "valid|expiring|expired|pending_dns|issuing|failed|none",
    "cert_name": "shop.example.com",
    "domains": ["shop.example.com", "www.shop.example.com"],
    "expires": "2027-01-12T10:00:00Z", "days_left": 60, "auto_renew": true,
    "last_error": null
  },
  "proxy": null,
  "redirect": null,
  "options": { "...": "see §8" },
  "basic_auth": {"enabled": false, "users": []},
  "drift": {"status": "clean|drifted|unknown", "checked_at": "…", "files": []},
  "dns": {"status": "points_here", "checked_at": "…"},
  "git": {"branch": "main", "commit": "a1f3c9e", "message": "Fix VAT", "remote": "git@github.com:org/shop.git"},
  "ssl_expires": "2027-01-12T10:00:00Z",
  "root": "/var/www/shop.example.com/current/public",
  "proxy_pass": null,
  "config_path": "/etc/nginx/sites-enabled/shop.example.com.conf",
  "created_at": "…", "adopted_at": null
}
```
- `managed`: NexaCloud renders the nginx file from the model, and that render is the
  only write path. `unmanaged`: imported as a record only. Read-only, except the raw
  editor (sudo) and adopt. `detached`: was managed, the operator chose to edit the full
  file raw. The model is kept but not rendered, and the site behaves like `unmanaged`.
- `proxy` = `{"upstream_host":"127.0.0.1","port":8080,"websocket":true,"read_timeout_s":3600}`.
- `redirect` = `{"to":"https://shop.example.com","code":301,"preserve_path":true}` (redirect-only sites).
- **Paths for new sites:**
  - `base` is `/var/www/<id>`. In releases mode `app_root` is `<base>/current`, and in
    in-place mode it is `<base>`.
  - `web_root` is `<app_root>/<web_root_rel>`. `web_root_rel` defaults to `public` for
    laravel, `public` (editable, e.g. `public_html`) for php, and `.` (editable, e.g.
    `dist`) for static. It must match `^[A-Za-z0-9._/-]{0,128}$`, contain no `..`, and
    canonicalize inside `app_root`.
- `kind` is detected for unmanaged sites with the v0.1 rules. `redirect` is a new
  detected kind: a server block whose only content is `return 301|302 …`.

`GET /api/sites` stays an array (v0.1 fields plus `managed`, `state`, `slug`, `runtime`,
`ssl.status`, `dns.status`). Unmanaged entries still come from the live nginx scan. Rows
in `sites` win for matching ids.

## 2. Domains list and DNS

### 2.1 `GET /api/domains` (P1)
```json
{
  "server_ips": {"ipv4": ["203.0.113.10"], "ipv6": ["2001:db8::10"]},
  "records_hint": [{"type": "A", "name": "@", "value": "203.0.113.10"}, {"type": "A", "name": "*", "value": "203.0.113.10"}],
  "domains": [
    {"name": "nexadev.io", "site_id": "nexadev.io", "role": "primary|alias|redirect|subdomain|proxy",
     "serves": "website → /public", "dns": {"status": "points_here", "records": ["203.0.113.10"], "checked_at": "…"},
     "ssl": {"status": "valid", "days_left": 74}, "php_version": "8.3", "managed": true}
  ]
}
```
- `role` is `subdomain` when the name has more labels than its registrable parent (PSL
  lookup via the `psl` crate) and is a site's primary. It is `primary` for an apex
  primary, `alias` or `redirect` for a non-primary server_name, and `proxy` for proxy
  sites.
- `server_ips` comes from global-scope addresses (`ip -j addr show scope global`), plus
  config overrides `public_ipv4` / `public_ipv6` for NAT. It never calls third-party
  IP-echo services.

### 2.2 DNS check
`POST /api/dns/check` (P1, class S: it only updates cached state). The body is
`{"names": ["a.example.com"]}` or `{}` for all.

Response:
`{"results":[{"name","status","a":[…],"aaaa":[…],"cname":null,"caa_ok":true,"checked_at"}]}`

- The resolver is `hickory-resolver` querying `1.1.1.1` and `8.8.8.8` directly, bypassing
  the local cache. Timeout 3 s per query.
- `status`:
  - `points_here`: every A/AAAA record is one of `server_ips`. An absent AAAA is fine.
  - `partial`: some A/AAAA records point here and some do not.
  - `not_pointing`: no record points here.
  - `proxied`: all records are inside Cloudflare's published ranges, embedded in the
    binary. HTTP-01 still works; the UI shows an info pill.
  - `no_record`: NXDOMAIN or no A/AAAA.
  - `error`: the lookup failed.
- `caa_ok`: no CAA record, or a CAA record allows `letsencrypt.org`.
- The background checker re-checks names in `pending_dns` every 5 min and every other
  name every 6 h. When a name turns `points_here` and its site has `ssl.status =
  pending_dns`, it enqueues certificate issuance (§5).

## 3. Import and adopt

Import makes an existing site **known**. Adopt makes it **managed**.

### 3.1 `GET /api/import/scan` (P1)
A read-only scan, with no writes. It runs synchronously, with an 8 s cap.
```json
{"duration_ms": 4200, "items": [
  {"id": "site:shop.nexadev.io", "type": "site", "name": "shop.nexadev.io", "kind": "laravel", "framework": "Laravel 12",
   "detail": "/var/www/shop · artisan found · PHP 8.3", "default_selected": true, "imported": false},
  {"id": "db:mysql", "type": "databases", "name": "MySQL databases", "kind": "5 found", "detail": "shop_prod, …", "default_selected": true},
  {"id": "db:postgres", "type": "databases", "name": "PostgreSQL databases", "kind": "1 found", "detail": "api_prod", "default_selected": true},
  {"id": "supervisor", "type": "supervisor", "name": "Supervisor programs", "kind": "6 workers", "detail": "/etc/supervisor/conf.d/*.conf", "default_selected": true},
  {"id": "cron", "type": "cron", "name": "Cron jobs", "kind": "4 entries", "detail": "schedule:run for 4 Laravel apps", "default_selected": true},
  {"id": "certs", "type": "certs", "name": "SSL certificates", "kind": "certbot", "detail": "8 certificates in /etc/letsencrypt/live", "default_selected": true},
  {"id": "site:default", "type": "site", "name": "default nginx site", "kind": "Unused", "detail": "/etc/nginx/sites-enabled/default", "default_selected": false}
]}
```
- **Sources:**
  - Sites: the v0.1 scan. `framework` comes from the `laravel/framework` version in
    `<app>/composer.lock`.
  - Supervisor: `/etc/supervisor/conf.d/*.conf`, parsed with `program:` sections
    mapped to sites by `directory` or `command` path prefix.
  - Cron: `/etc/cron.d/*`, `/var/spool/cron/crontabs/*`, with lines containing
    `artisan schedule:run` mapped to sites by path.
  - Certs: `certbot certificates`, parsed, with `cert_name → domains, expiry, path`.
- **Labels:** `Unused` is the label for Ubuntu's `default` site when no other site
  needs it as `default_server`.

### 3.2 Import (record only)
| Method | Path | Class | Sudo | Locks | Notes |
|---|---|---|---|---|---|
| POST | `/api/import/preview` | C (no file writes) | – | – | Body: `{"items":["site:shop.nexadev.io","db:mysql",…]}`. The plan's `files` is empty and `data.records` lists the rows to create. |
| POST | `/api/import/apply` | C | – | – | Creates `sites` rows with `state:unmanaged`, links supervisor programs and cron lines to sites (`external:true`), and links certs. The job result is `{"imported": n}`. |
| DELETE | `/api/import/{id}` | S | – | – | "Undo import". It removes NexaCloud's record only. Refused with 409 if the site is `managed`; un-adopt first via change rollback. |

Disabling Ubuntu's `default` site is **not** part of import. It is an advisor rule (no
fix in P1, info only).

### 3.3 Adopt (unmanaged → managed)
| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/sites/{id}/adopt/preview` | C | – | – |
| POST | `/api/sites/{id}/adopt/apply` | C | – | `site:{id}`, `nginx`, + `php:<ver>` if `own_pool` |

Body:
```json
{"kind": "laravel", "deploy_mode": "in_place", "own_pool": false, "options": null, "plan_hash": "…"}
```
`plan_hash` is sent on apply only.

**Plan.**
1. Parse the existing server blocks for this site with `nginx.rs`. Build a model from
   `server_names`, `root`, the `fastcgi_pass` socket → `php_version`, the SSL paths
   (`/etc/letsencrypt/live/<name>/` → `issuer: letsencrypt`, `cert_name`),
   `client_max_body_size`, gzip, `add_header`s, `return 301` blocks → `redirects`, and
   `proxy_pass` → `proxy`.
2. Every directive that does not map to a model field goes into `data.unmapped_directives`
   (with line numbers). It is copied **verbatim** into the new `custom.conf` (location
   blocks included) so behaviour is preserved.
3. `options` are inferred from what exists (e.g. `http2` already on → `true`). Passing
   `options` overrides this.
4. Render the template (§7). The file diff is **old file → new file**. If the old
   file's name differs (e.g. `sites-available/shop`), the plan deletes the old file and
   its `sites-enabled` symlink, and creates `<id>.conf` and its symlink.
5. With `own_pool:true` (restart tier), it also creates a per-site pool for the existing
   owner user and switches the socket. It does **not** touch `www.conf`.
6. Verify (data: `verify`): before apply, record `GET /` status and `Content-Type` for
   each server_name over 127.0.0.1 with SNI. After reload, the same requests must give
   the same status class and Content-Type. If not, auto-restore (Phase 0 pipeline).

Warnings:
- `if` blocks
- multiple sites in one file. The plan splits them, and the other site's block moves
  to its own `<other>.conf` as `unmanaged`.
- `include` of non-standard files (kept in `custom.conf`)
- `listen` with `default_server`. This is kept via `options.default_server:true`.

## 4. Create site, subdomain, redirect and proxy

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/sites/preview` | C | ✓* if `basic_auth` set | – |
| POST | `/api/sites/apply` | C | ✓* same | `site:{id}`, `nginx`, `php:<ver>` (php/laravel), `certbot` (if `ssl`, taken by the follow-up issuance job) |
| POST | `/api/sites/{id}/delete/preview` | C | ✓ | – |
| POST | `/api/sites/{id}/delete/apply` | C | ✓ | `site:{id}`, `nginx`, `php:<ver>`, `cron`, `supervisor`, `certbot` |
| POST | `/api/sites/{id}/domains/preview` / `apply` | C | – | `site:{id}`, `nginx`, `certbot` |
| POST | `/api/sites/{id}/basic-auth/preview` / `apply` | C | ✓ | `site:{id}`, `nginx` |

### 4.1 Create body
```json
{
  "kind": "laravel|php|static|redirect|proxy",
  "domain": "blog.example.com",
  "aliases": [],
  "www": "none|redirect_to_primary|alias",
  "php_version": "8.4",
  "web_root": "public",
  "deploy_mode": "releases|in_place|none",
  "ssl": true,
  "redirect": {"to": "https://example.com", "code": 301, "preserve_path": true},
  "proxy": {"port": 8080, "websocket": true},
  "basic_auth": {"username": "staging", "password": "…"},
  "options": {"client_max_body_size": "64M"},
  "clone_from": null
}
```
Validation:
- `domain`, `aliases[]`: `HOSTNAME`. They must not be any existing site's server_name
  (checked across the live nginx scan, not just SQLite), else `409 conflict`.
- `www`: only when `domain` has exactly 2 labels or is a PSL registrable domain. It adds
  `www.<domain>`.
- `php_version` must be installed (`/etc/php/<v>/fpm` exists), else `424 not_installed`.
- `redirect.to`: `^https?://[A-Za-z0-9.-]+(:[0-9]{1,5})?(/[A-Za-z0-9._~%!&'()*+,;=:@/-]*)?$`.
  No `$`, `"`, `{`, `}`, spaces, `?` or `#` (no nginx variable injection).
  `preserve_path:true` appends `$request_uri`.
- `redirect.code`: `301|302|307|308`.
- `proxy.port`: 1024–65535 and not the panel port. The upstream host is always
  `127.0.0.1`; remote upstreams are not allowed.
- `basic_auth.username`: `^[A-Za-z0-9._-]{1,32}$`. `password`: 8–128 chars, no `:` or newline.
- `clone_from` (P2): `{"site_id":"shop.example.com","branch":"develop"}`. This creates a
  project with the same repo and a new branch. See projects-deploys.md §3.

### 4.2 Plan (kind laravel/php/static)
1. `useradd --system --user-group --home-dir /var/www/<id> --no-create-home --shell /usr/sbin/nologin nc_<slug>`
2. Create the directories:
   - `install -d -o nc_<slug> -g nc_<slug> -m 0755 /var/www/<id>`
   - in releases mode, also `…/releases` and `…/shared`
   - for laravel: `…/shared/storage/{app/public,framework/{cache/data,sessions,views},logs}` (0775)
   - a placeholder `index.html` (static) or `index.php` (php/laravel). The placeholder
     is `<?php http_response_code(200); echo 'NexaCloud: site ready';`. For releases
     mode it is placed in `releases/000000-placeholder/<web_root>/` with
     `current -> releases/000000-placeholder`.
3. The PHP pool `/etc/php/<v>/fpm/pool.d/<slug>.conf` (php-services.md §3.1), then
   `php-fpm<v> -t`, then `systemctl reload php<v>-fpm`.
4. `install -d -m 0755 /etc/nginx/nexacloud/sites/<id>`. `custom.conf` is created empty
   with a header comment.
5. The vhost `/etc/nginx/sites-available/<id>.conf`, plus the symlink
   `/etc/nginx/sites-enabled/<id>.conf`. This is rendered **HTTP-only** (template
   variant `ssl:false`) when no cert exists yet.
6. Shared snippets and the http-context file (§7.2), created if missing or outdated.
   Same change.
7. `nginx -t` → `systemctl reload nginx`.
8. Verify: `GET http://<id>/` resolved to 127.0.0.1 must not return 5xx.
9. If `ssl:true`, the job then enqueues §5 issuance as a **separate** change, which
   needs DNS. If DNS is not pointing, `ssl.status = pending_dns`.

`redirect` and `proxy` kinds skip steps 1–3. The **subdomain** form in the Domains
screen is this same endpoint.

**Delete.** Body `{"confirm":"<id>","remove_files":false,"remove_user":false,"revoke_cert":false}`.
- It removes the nginx file and symlink, the pool, cron, supervisor programs and units.
- `remove_files` deletes `/var/www/<id>`. It is `reversible:false`, and the UI warns.
- The cert is deleted with `certbot delete --cert-name <id> --non-interactive` only when
  `revoke_cert` is set.

## 5. Let's Encrypt

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/sites/{id}/ssl/preview` | C | – | – |
| POST | `/api/sites/{id}/ssl/apply` | C | – | `site:{id}`, `nginx`, `certbot` |

Body: `{"action":"issue|renew|remove","domains":["shop.example.com","www.shop.example.com"]}`.
`domains` defaults to all server_names.

**One-time setup** (part of the first change that needs it):
- `/var/lib/nexacloud/acme/.well-known/acme-challenge/`, dir 0755, owned by root.
- The ACME snippet (§7.2).
- The deploy hook `/etc/letsencrypt/renewal-hooks/deploy/nexacloud-reload-nginx` (0755):
  ```sh
  #!/bin/sh
  # Managed by NexaCloud
  nginx -t -q && systemctl reload nginx
  ```
- Check that `certbot.timer` is enabled. If it is not,
  `systemctl enable --now certbot.timer`.

**Issue flow:**
1. Precheck: every domain has `dns.status ∈ {points_here, proxied}` and `caa_ok`, else
   `422 precondition_failed`.
2. A port-80 block that includes the ACME snippet must exist; it does in every template.
3. Run, as argv:
   ```
   certbot certonly --webroot -w /var/lib/nexacloud/acme --cert-name <id>
     -d <d1> -d <d2> … --non-interactive --agree-tos
     (-m <admin_email> | --register-unsafely-without-email)
     --key-type ecdsa --elliptic-curve secp256r1 --expand --keep-until-expiring
     [--server <acme_server>]
   ```
   `acme_server` is a config key. The default is unset, which means LE production. The
   e2e suite sets it to the Pebble URL plus `REQUESTS_CA_BUNDLE`.
4. Re-render the vhost with `ssl:true` (cert
   `/etc/letsencrypt/live/<id>/fullchain.pem`, key `…/privkey.pem`), then `nginx -t`,
   reload, and verify that `https://<id>/` over 127.0.0.1 with SNI returns no 5xx and
   serves the new cert serial.
5. Failure handling:
   - Throttle: at most 3 failed attempts per name per hour, tracked in SQLite. Let's
     Encrypt itself allows 5 failed validations per hour.
   - `ssl.status = failed`, and `ssl.last_error` holds certbot's last 20 lines.

**Renew** forces `certbot renew --cert-name <id> --force-renewal --non-interactive`.
**Remove** switches the vhost to HTTP-only and keeps the cert files.

The **advisor rule `cert-expiry`** (advisor.md) uses `days_left`: under 14 is a warning,
under 0 is critical. Its fix is `ssl/apply {action:"renew"}`.

## 6. Reading and testing nginx config

`GET /api/sites/{id}/nginx` (P1):
```json
{"path": "/etc/nginx/sites-available/shop.example.com.conf", "managed": true, "state": "managed",
 "content": "…full file…", "custom_conf": {"path": "…/custom.conf", "content": ""},
 "options": {…}, "template": "laravel", "template_version": 1,
 "last_test": {"ok": true, "output": "nginx: configuration file … syntax is ok", "at": "…"},
 "last_change_id": "…", "drift": {"status": "clean"}}
```

`POST /api/sites/{id}/nginx/test` (P1, class S, no system writes). Body:
`{"kind":"options|custom_conf|raw","options":{…}|"content":"…"}`.

1. Copy `/etc/nginx` into `/run/nexacloud/nginx-test-<uuid>/` (tmpfs).
2. Rewrite absolute include paths (`/etc/nginx/` → the temp root) in the copied files.
3. Write the candidate file into the copy.
4. Run `nginx -t -p /run/nexacloud/nginx-test-<uuid>/ -c /run/nexacloud/nginx-test-<uuid>/nginx.conf`.
5. Return `{"ok":bool,"output":"…"}`, then delete the copy.

This is the mockup's "Test config" button. It never touches live files.

## 7. nginx templates

### 7.1 Rendering rules
- **Template variables:**

  | Variable | Value |
  |---|---|
  | `{{id}}` | site id |
  | `{{names}}` | server_names joined by spaces |
  | `{{primary}}` | primary domain |
  | `{{root}}` | web root |
  | `{{socket}}` | PHP-FPM socket |
  | `{{port}}` | proxy port |
  | `{{body}}` | `client_max_body_size` |
  | `{{custom}}` | the site's `custom.conf` path |
- **HTTPS listen line:**
  - nginx **< 1.25.1** (Ubuntu 22.04's 1.18 and 24.04's 1.24): `listen 443 ssl http2;`
    when `options.http2`, else `listen 443 ssl;`.
  - nginx **≥ 1.25.1**: `listen 443 ssl;` plus a separate `http2 on;` line.
  - The version is read from `nginx -v` (stderr) at render time.
- **IPv6:** add `listen [::]:…` lines only if `/proc/net/if_inet6` exists.
- **Header comment:** the first line is always
  `# Managed by NexaCloud — changes here are overwritten. Use custom.conf.`. The second
  is `# nexacloud: site=<id> template=<t> version=<n> sha=<hash of model>`.
- **Indentation:** 4 spaces. LF line endings. The file ends with a newline.

### 7.2 Shared files (one per server, rendered once)

`/etc/nginx/conf.d/zz-nexacloud.conf` (http context):
```nginx
# Managed by NexaCloud
log_format nexacloud '$remote_addr - [$time_iso8601] "$request" $status $body_bytes_sent '
                     '$request_time "$upstream_response_time" "$http_referer" "$http_user_agent" $host';
map $http_upgrade $nc_connection_upgrade { default upgrade; '' close; }
limit_req_zone $binary_remote_addr zone=nc_login:10m rate=10r/m;
server_tokens off;
```
The names (`nexacloud`, `$nc_connection_upgrade`, `nc_login`) are prefixed so they
cannot collide with existing `map`/`zone` definitions.

**Ownership with Phase 0.** Phase 0's `nginx-hardening` change already creates this
file, writing only `server_tokens off;`, and only when it is not set elsewhere at http
level. Phase 1 extends that **same renderer**, adding a `managed_settings` row per
directive. A site change that first needs `log_format`/`map`/`limit_req_zone` adds them
to the plan as a `modify` of this file. `server_tokens off;` is present only when the
`nginx-hardening` change has been applied.

Before adding a directive, check the effective config (`nginx -T`) for an existing
http-level `log_format nexacloud`, `map … $nc_connection_upgrade` or `zone=nc_login`. A
conflict with one of these is a `409 conflict`.

`/etc/nginx/snippets/nexacloud-acme.conf`:
```nginx
location ^~ /.well-known/acme-challenge/ {
    root /var/lib/nexacloud/acme;
    default_type "text/plain";
    try_files $uri =404;
}
```
`/etc/nginx/snippets/nexacloud-ssl.conf`:
```nginx
ssl_protocols TLSv1.2 TLSv1.3;
ssl_prefer_server_ciphers off;
ssl_session_cache shared:NexaSSL:10m;
ssl_session_timeout 1d;
ssl_session_tickets off;
```
`/etc/nginx/snippets/nexacloud-security-headers.conf` (HSTS is a separate line, only when SSL is on):
```nginx
add_header X-Frame-Options "SAMEORIGIN" always;
add_header X-Content-Type-Options "nosniff" always;
add_header Referrer-Policy "strict-origin-when-cross-origin" always;
add_header Permissions-Policy "camera=(), microphone=(), geolocation=()" always;
```
`/etc/nginx/snippets/nexacloud-gzip.conf`:
```nginx
gzip on;
gzip_vary on;
gzip_proxied any;
gzip_comp_level 5;
gzip_min_length 1024;
gzip_types text/plain text/css text/xml application/json application/javascript application/xml
           application/rss+xml application/atom+xml image/svg+xml font/ttf font/otf application/vnd.ms-fontobject;
```
`/etc/nginx/snippets/nexacloud-maintenance.conf` (included by every content site; it is
a no-op unless the flag file exists, and toggling the flag needs no reload):
```nginx
error_page 503 /__nexacloud_maintenance.html;
location = /__nexacloud_maintenance.html { root /usr/share/nexacloud; internal; }
```
In each vhost, at server level before the locations, add
`if (-f /var/lib/nexacloud/maintenance/{{id}}) { return 503; }`.

**`add_header` inheritance:** no template `location` uses `add_header`. Caching uses
`expires`, so the server-level headers are inherited everywhere. Anything in
`custom.conf` that adds `add_header` inside a location breaks this inheritance. The raw
editor warns about it.

### 7.3 Common blocks (`ssl:true` variant)
**Port 80** (all kinds). With `force_https:false` and `ssl:true`, the 80 block instead
serves the site exactly like the 443 block, without the ssl lines.
```nginx
server {
    listen 80;
    listen [::]:80;
    server_name {{names}};
    include /etc/nginx/snippets/nexacloud-acme.conf;
    location / { return 301 https://{{primary}}$request_uri; }
}
```
**Redirect alias block**, one per `redirects[]` entry on content sites. It shares the cert:
```nginx
server {
    listen 443 ssl http2;
    listen [::]:443 ssl http2;
    server_name {{redirect.from}};
    ssl_certificate     /etc/letsencrypt/live/{{id}}/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/{{id}}/privkey.pem;
    include /etc/nginx/snippets/nexacloud-ssl.conf;
    return {{code}} {{to}}$request_uri;
}
```
**HTTPS head**. In the templates below it stands in for the lines marked `# HEAD`:
```nginx
    listen 443 ssl http2;
    listen [::]:443 ssl http2;
    server_name {{primary}} {{aliases}};
    ssl_certificate     /etc/letsencrypt/live/{{id}}/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/{{id}}/privkey.pem;
    include /etc/nginx/snippets/nexacloud-ssl.conf;
    include /etc/nginx/snippets/nexacloud-security-headers.conf;      # options.security_headers
    add_header Strict-Transport-Security "max-age=31536000" always;     # options.security_headers && ssl
    include /etc/nginx/snippets/nexacloud-gzip.conf;                    # options.gzip
    client_max_body_size {{body}};
    access_log /var/log/nginx/{{id}}.access.log nexacloud;
    error_log  /var/log/nginx/{{id}}.error.log warn;
    include /etc/nginx/snippets/nexacloud-maintenance.conf;
    if (-f /var/lib/nexacloud/maintenance/{{id}}) { return 503; }
    # auth_basic "Restricted"; auth_basic_user_file /etc/nginx/nexacloud/sites/{{id}}/htpasswd;   # basic_auth
```
In the HTTP-only variant (`ssl:false`), the port-80 block contains the HEAD (minus the
ssl, HSTS and http2 lines), the ACME include, and the body. No 443 block is rendered.

### 7.4 `laravel`
```nginx
server {
    # HEAD
    root {{root}};
    index index.php;
    charset utf-8;

    location = /favicon.ico { access_log off; log_not_found off; }
    location = /robots.txt  { access_log off; log_not_found off; }

    location ^~ /build/ { expires 30d; access_log off; try_files $uri =404; }      # options.static_cache

    location = /login {                                                              # options.rate_limit_login
        limit_req zone=nc_login burst=5 nodelay;
        limit_req_status 429;
        try_files $uri /index.php?$query_string;
    }

    location / {
        try_files $uri $uri/ /index.php?$query_string;
    }

    error_page 404 /index.php;

    location ~ ^/index\.php(/|$) {
        fastcgi_pass unix:{{socket}};
        fastcgi_split_path_info ^(.+\.php)(/.*)$;
        include fastcgi_params;
        fastcgi_param SCRIPT_FILENAME $realpath_root$fastcgi_script_name;
        fastcgi_param DOCUMENT_ROOT $realpath_root;
        fastcgi_hide_header X-Powered-By;
        fastcgi_read_timeout {{request_timeout}}s;
    }

    location ~ \.php$ { return 404; }

    location ~ /\.(?!well-known).* { deny all; }                                     # options.block_hidden

    include {{custom}};
}
```
Notes:
- Only `index.php` runs; any other `.php` returns 404 (Laravel's documented config).
- `$realpath_root` makes releases-mode symlink switches safe.
- `rate_limit_login.path` defaults to `/login` and is configurable. It must match
  `^/[A-Za-z0-9/_-]{0,64}$`.

### 7.5 `php` (plain PHP)
This is the same as laravel except:
```nginx
    root {{root}};
    index index.php index.html;

    location ~* \.(?:css|js|mjs|woff2?|ttf|svg|png|jpe?g|gif|webp|avif|ico)$ { expires 30d; access_log off; try_files $uri =404; }   # static_cache

    location / {
        try_files $uri $uri/ /{{front_controller}}?$query_string;   # front_controller default index.php; if "none": try_files $uri $uri/ =404;
    }

    location ~ \.php$ {
        try_files $fastcgi_script_name =404;
        fastcgi_pass unix:{{socket}};
        fastcgi_split_path_info ^(.+\.php)(/.*)$;
        include fastcgi_params;
        fastcgi_param SCRIPT_FILENAME $realpath_root$fastcgi_script_name;
        fastcgi_param DOCUMENT_ROOT $realpath_root;
        fastcgi_hide_header X-Powered-By;
        fastcgi_read_timeout {{request_timeout}}s;
    }
```
`front_controller` must match `^([A-Za-z0-9_-]{1,64}\.php|none)$`.

### 7.6 `static`
```nginx
server {
    # HEAD
    root {{root}};
    index index.html;
    location ~* \.(?:css|js|mjs|woff2?|ttf|svg|png|jpe?g|gif|webp|avif|ico)$ { expires 30d; access_log off; try_files $uri =404; }   # static_cache
    location / { try_files $uri $uri/ {{fallback}}; }    # fallback: "=404" or "/index.html" (options.spa_fallback)
    location ~ /\.(?!well-known).* { deny all; }          # block_hidden
    include {{custom}};
}
```

### 7.7 `proxy`
```nginx
server {
    # HEAD
    location / {
        proxy_pass http://127.0.0.1:{{port}};
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-Host $host;
        proxy_set_header Upgrade $http_upgrade;                 # proxy.websocket
        proxy_set_header Connection $nc_connection_upgrade;     # proxy.websocket
        proxy_read_timeout {{proxy.read_timeout_s}}s;           # default 60; 3600 when websocket
        proxy_buffering on;
    }
    include {{custom}};
}
```
FrankenPHP sites use a variant of this template. See frankenphp.md §6.

### 7.8 `redirect` (redirect-only site)
```nginx
server {
    listen 80;
    listen [::]:80;
    server_name {{names}};
    include /etc/nginx/snippets/nexacloud-acme.conf;
    location / { return {{code}} {{to}}{{$request_uri if preserve_path}}; }
}
server {
    listen 443 ssl http2;
    listen [::]:443 ssl http2;
    server_name {{names}};
    ssl_certificate     /etc/letsencrypt/live/{{id}}/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/{{id}}/privkey.pem;
    include /etc/nginx/snippets/nexacloud-ssl.conf;
    return {{code}} {{to}}{{$request_uri if preserve_path}};
}
```

## 8. Visual options ↔ directives

`POST /api/sites/{id}/options/preview` and `/apply` (P1, class C, no sudo, locks
`site:{id}` + `nginx`). Managed sites only, else `409 unmanaged`.

The body is a partial `options` object. Unspecified keys keep their values.

| Option (UI label) | Type / validation | Default (new site) | Directive effect |
|---|---|---|---|
| `force_https` (Force HTTPS) | bool | true | Port-80 block is `return 301 https://…` vs serving content. Ignored when there is no cert. |
| `http2` (HTTP/2) | bool | true | Listen line, see §7.1 |
| `gzip` (Gzip compression) | bool | true | `include …/nexacloud-gzip.conf` |
| `static_cache` (Cache static assets) | bool | true | laravel: `location ^~ /build/ { expires 30d; … }`. php/static: the extension regex location. |
| `security_headers` (Security headers) | bool | true | `include …/nexacloud-security-headers.conf`, plus HSTS when SSL is on |
| `hsts_include_subdomains` | bool | false | Appends `; includeSubDomains` to HSTS. Warning: affects every subdomain. |
| `rate_limit_login` (Rate limit /login) | bool, + `rate_limit_path` | false, `/login` | `location = <path> { limit_req zone=nc_login burst=5 nodelay; … }` |
| `block_hidden` (Block hidden files) | bool | true | `location ~ /\.(?!well-known).* { deny all; }` |
| `client_max_body_size` (Max upload size) | `^[1-9][0-9]{0,3}[MG]$`, ≤ 2G; UI offers 64M/128M/256M | `64M` | `client_max_body_size`. If larger than the pool's `upload_max_filesize`/`post_max_size`, the plan **also** updates the pool's `php_admin_value[...]` and lists the extra file. |
| `request_timeout` | int 10–600 | 120 | `fastcgi_read_timeout`. Also the pool's `request_terminate_timeout`. |
| `spa_fallback` | bool (static only) | false | `try_files … /index.html` |
| `front_controller` | see §7.5 (php only) | `index.php` | `try_files` target |
| `default_server` | bool | false | Adds `default_server` to the listen lines. At most one site. |

Basic auth is set through `/api/sites/{id}/basic-auth/{preview,apply}`, not options,
because it carries a secret.
- Body: `{"enabled":true,"users":[{"username":"u","password":"…"}]}`.
- Hashes are made with `openssl passwd -6 -stdin` (SHA-512 crypt; password on stdin).
  The salt is generated at apply (README rule R1).
- The file is `/etc/nginx/nexacloud/sites/<id>/htpasswd`, mode 0640, `root:www-data`.
- The diff is marked `secret:true`.

## 9. Raw editor and custom.conf

| Method | Path | Class | Sudo | Locks | Applies to |
|---|---|---|---|---|---|
| POST | `/api/sites/{id}/custom-conf/preview` / `apply` | C | – | `site:{id}`, `nginx` | managed sites. Body `{"content":"…"}`, ≤ 64 KiB. |
| POST | `/api/sites/{id}/raw/preview` / `apply` | C | ✓ | `site:{id}`, `nginx` | unmanaged/detached sites, or managed sites with `detach:true`. Body `{"content":"…","detach":false}`, ≤ 256 KiB. |
| POST | `/api/sites/{id}/reapply/preview` / `apply` | C | – | `site:{id}`, `nginx` | Re-renders a detached or drifted site from its model, which makes it managed again. |

- `custom.conf` is included **inside** the main HTTPS server block (§7). It is for
  server-context directives and extra `location` blocks.
- The plan rejects `server {`, `http {`, `include` of paths outside `/etc/nginx` or
  `/etc/letsencrypt`, and `ssl_certificate*` with `400 invalid`. These checks are
  against tokenised content using `nginx.rs`.

**Write path** (the Phase 0 pipeline, used by every nginx-writing change here):
1. Precheck: `nginx -t` on the live config must pass, else `409 nginx_broken`.
2. Snapshot the target files.
3. Write `<path>.nc-tmp`, fsync, then `rename()` over the target.
4. Run `nginx -t`. On failure, restore and report the error output.
5. Run `systemctl reload nginx`.
6. Probe as in §3.3 step 6. On failure, restore and reload.

## 10. Drift detection

- **What is tracked.** For each managed site, store the rendered `sha256` of every
  managed file: vhost, custom.conf, htpasswd, pool, cron, supervisor, FrankenPHP
  unit/Caddyfile. Also track the `sites-enabled` symlink target.
- **When it is checked:**
  - every 5 min
  - on `GET /api/sites/{id}`
  - before any change on the site. If the site has drifted, the change returns
    `409 drifted` unless `"accept_drift":true` is in the body.
- **Result.** `drift.files[] = {"path","expected_sha256","actual_sha256"|null,"diff"}`,
  where `diff` is rendered vs actual, unified, and limited to 64 KiB.
- **Not drift:** `ssl_certificate` targets changed by certbot itself, i.e. the same
  `live/<id>` path.
- `GET /api/sites/{id}/drift` (P1) returns the `drift` object, after running a check.
- `POST /api/sites/{id}/drift/accept` (P1, class S, sudo):
  - managed vhost: marks the site `detached`; the actual file is kept.
  - pool, cron and other files: records the new hash as expected.
- To overwrite the drifted files with the managed version, use **reapply** (§9).

## 11. Panel domain

Serves the panel at `https://panel.example.com` through nginx. The `:8443` listener is
kept as break-glass.

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| GET | `/api/panel/domain` | – | – | – |
| POST | `/api/panel/domain/preview` / `apply` | C | ✓ | `panel`, `nginx`, `certbot` |

- GET returns `{"domain":"panel.example.com"|null,"ssl":{…},"dns":{…},"break_glass":"https://203.0.113.10:8443"}`.
- The body is `{"domain":"panel.example.com"}`, or `{"domain":null}` to remove.
- **Plan:**
  - A site row `id = <domain>`, `kind = proxy`, `managed`, internal flag `panel:true`,
    which hides it from the Projects list.
  - Template = `proxy` with these differences:
    ```nginx
    location / {
        proxy_pass https://127.0.0.1:8443;
        proxy_ssl_verify off;            # loopback to the agent's self-signed cert
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto https;
        proxy_buffering off;             # SSE (jobs, log tails)
        proxy_read_timeout 3600s;
        client_max_body_size 2G;         # SQL import uploads
    }
    ```
  - When the agent runs with `tls=false` (dev), the scheme is `http://`.
  - SSL issuance (§5) is required. DNS must point here first.
  - `config.toml` gains `public_url = "https://panel.example.com"`. It is used for
    webhook URLs and Actions YAML, and as the allowed Origin.
- The Origin check is unchanged, because nginx passes the original `Host`.
- `trusted_proxies` behaviour is described in README §1.1.
