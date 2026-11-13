# NexaCloud API contract (v0.1)

The Rust agent (`agent/`) serves this JSON API under `/api` and the built Nuxt SPA (`web/.output/public`, installed to `/opt/nexacloud/web`) for every other path (SPA fallback to `index.html`).

All responses are JSON. Errors: HTTP status + `{"error": "human readable message"}`.
All sizes are bytes (u64). All timestamps are RFC 3339 strings in UTC.
Every endpoint except `GET /api/health` and `POST /api/auth/login` requires a valid session cookie, else `401 {"error":"unauthorized"}`.
Mutating requests (POST/PUT/DELETE) must carry an `Origin` header whose host matches the `Host` header, else `403`.

## Auth

| Method | Path | Body | Response |
|---|---|---|---|
| GET | `/api/health` | – | `{"ok":true,"version":"0.1.0"}` (public) |
| POST | `/api/auth/login` | `{"password":"..."}` | `200 {"ok":true}` + `Set-Cookie: nc_session=<token>; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=43200`. Wrong password: `401 {"error":"wrong password"}`. More than 5 failures from one IP in 15 min: `429 {"error":"too many attempts, try again later"}` |
| POST | `/api/auth/logout` | – | `{"ok":true}` and clears cookie |
| GET | `/api/auth/me` | – | `{"ok":true}` |

`Secure` is omitted when the agent runs with TLS disabled (dev only).

## Overview

`GET /api/overview`
```json
{
  "hostname": "srv123",
  "alerts": [
    {"level": "critical|warning|info", "title": "PHP 8.1 no longer gets security fixes", "detail": "Used by: portal.example.com", "link": "/php"}
  ],
  "counts": {"sites": 7, "databases": 6, "services_down": 0}
}
```
Alert rules (computed server-side):
- critical: a PHP version ≤ 8.1 is installed and used by at least one site (`link: /php`)
- warning: PHP 8.2 in use (security support ends 2026-12-31)
- critical: UFW not installed or inactive (`link: /security`)
- critical: a known service that is installed is not active (`link: /services`)
- warning: an SSL certificate expires in < 14 days; critical if already expired (`link: /sites`)
- warning: any disk mount > 85 % used (`link: /`)
- info: fail2ban not installed (`link: /security`)

## System

`GET /api/system`
```json
{
  "hostname": "srv123", "os": "Ubuntu 24.04.1 LTS", "kernel": "6.8.0-45-generic",
  "uptime_secs": 123456, "cpu_count": 2, "cpu_percent": 6.2, "load": [0.12, 0.2, 0.18],
  "memory": {"total": 8323477504, "used": 3495100416},
  "swap": {"total": 0, "used": 0},
  "disks": [{"mount": "/", "total": 107374182400, "used": 40802189312}]
}
```

## Services

`GET /api/services` → array
```json
[{"unit": "nginx", "label": "Nginx", "installed": true, "active": true, "state": "active", "version": "1.24.0"}]
```
Known units (only these are ever reported or controllable): `nginx`, `mysql`, `mariadb`, `postgresql`, `redis-server`, `supervisor`, `cron`, `fail2ban`, `ufw`, and every `phpX.Y-fpm` found on disk.

`POST /api/services/{unit}/restart` → `{"ok":true}` or error. `{unit}` must be in the known list (else 400).

## PHP

`GET /api/php`
```json
{
  "cli_default": "8.3",
  "versions": [
    {"version": "8.3", "fpm_installed": true, "fpm_active": true, "eol": false, "security_until": "2027-12-31", "sites": ["shop.example.com"]}
  ]
}
```
Detect versions from `/etc/php/*/` (dirs matching `^\d+\.\d+$`). `sites` = sites whose nginx config's `fastcgi_pass` socket contains `php{version}`.
Support table: 7.4→2022-11-28, 8.0→2023-11-26, 8.1→2025-12-31, 8.2→2026-12-31, 8.3→2027-12-31, 8.4→2028-12-31, 8.5→2029-12-31. `eol` = today > security_until.

## Sites (read-only scan of nginx)

`GET /api/sites` → array
```json
[{
  "id": "shop.example.com",
  "server_names": ["shop.example.com", "www.shop.example.com"],
  "root": "/var/www/shop/public",
  "kind": "laravel|php|static|proxy|unknown",
  "php_version": "8.3",
  "ssl": true,
  "ssl_expires": "2027-01-12T10:00:00Z",
  "proxy_pass": null,
  "config_path": "/etc/nginx/sites-enabled/shop.example.com",
  "git": {"branch": "main", "commit": "a1f3c9e", "message": "Fix VAT", "remote": "git@github.com:org/shop.git"}
}]
```
- Scan files in `/etc/nginx/sites-enabled/` (follow symlinks) and `/etc/nginx/conf.d/*.conf`. Parse `server_name`, `root`, `fastcgi_pass`, `proxy_pass`, `ssl_certificate`.
- `kind`: `laravel` if `<root>/../artisan` exists; `php` if fastcgi_pass present; `proxy` if proxy_pass; `static` if root and no php; else `unknown`.
- `ssl_expires`: read `ssl_certificate` PEM and parse notAfter (null if unreadable).
- `git`: from the project dir (`root/..` for laravel, else root) if `.git` exists, via `git -C <dir> rev-parse --abbrev-ref HEAD`, `git log -1 --format=%h%x1f%s`, `git remote get-url origin` (with `safe.directory=*`). Null otherwise.

## Databases

`GET /api/databases`
```json
{
  "mysql":    {"available": true, "version": "8.4.3", "databases": [{"name": "shop_prod", "size_bytes": 1288490188, "tables": 84}]},
  "postgres": {"available": false, "version": null, "databases": []}
}
```
MySQL via `mysql --batch --skip-column-names -e <query>` as root (unix socket auth). System schemas (`information_schema`, `mysql`, `performance_schema`, `sys`) excluded.
Postgres via `runuser -u postgres -- psql -At -c <query>`; exclude `template0`, `template1`, `postgres`.

`POST /api/databases/{engine}/{name}/backup` (`engine` = `mysql|postgres`, name must match `^[A-Za-z0-9_\-]{1,64}$` AND exist) → `{"job_id": "uuid"}`.
Dump to `{backup_dir}/{engine}/{name}/{name}-{YYYYmmdd-HHMMSS}.sql.gz` (mysqldump `--single-transaction --quick --routines`, pg_dump), file mode 0600.

## Backups

`GET /api/backups` → array, newest first
```json
[{"engine": "mysql", "database": "shop_prod", "file": "mysql/shop_prod/shop_prod-20261113-030000.sql.gz", "size_bytes": 212000000, "created_at": "2026-11-13T03:00:00Z"}]
```
`GET /api/backups/download?file=<relative file>` → streams the file as `application/gzip` with `Content-Disposition: attachment`. The path must canonicalize to inside `backup_dir` (else 400).

## Jobs

`GET /api/jobs` → last 50, newest first. `GET /api/jobs/{id}` →
```json
{"id": "uuid", "kind": "db_backup", "title": "Back up mysql/shop_prod", "status": "queued|running|ok|failed",
 "log": "text…", "started_at": "...", "finished_at": null}
```
Jobs are kept in memory (lost on restart) for v0.1.

## Security

`GET /api/security`
```json
{
  "ufw": {"installed": true, "active": false, "rules": ["22/tcp ALLOW Anywhere"]},
  "fail2ban": {"installed": false, "active": false},
  "unattended_upgrades": true,
  "ssh_password_auth": true
}
```
`ssh_password_auth` from `sshd -T` (`passwordauthentication yes`), null if unknown.

## Config file (`/etc/nexacloud/config.toml`)

```toml
listen = "0.0.0.0:8443"
web_dir = "/opt/nexacloud/web"
backup_dir = "/var/backups/nexacloud"
admin_password_hash = "$argon2id$..."
tls = true
tls_cert = "/etc/nexacloud/tls/cert.pem"
tls_key = "/etc/nexacloud/tls/key.pem"
```

## CLI (`nexacloud`)

- `nexacloud serve [--config PATH]` — run the agent (default config path `/etc/nexacloud/config.toml`).
- `nexacloud init [--config PATH]` — if the config does not exist: create it with a random 20-char admin password (argon2id hash stored), create a self-signed TLS cert/key (mode 0600) if missing, create `backup_dir`; print exactly one line `NEXACLOUD_ADMIN_PASSWORD=<password>` to stdout. If the config exists: print `config exists, keeping it` and change nothing.
- `nexacloud reset-password [--config PATH]` — generate a new password, update the hash, print `NEXACLOUD_ADMIN_PASSWORD=<password>`.
- `nexacloud --version`.
