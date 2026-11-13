# NexaCloud API contract (v0.2)

The Rust agent (`agent/`) serves this JSON API under `/api` and the built Nuxt SPA (`web/.output/public`, installed to `/opt/nexacloud/web`) for every other path (SPA fallback to `index.html`).

All responses are JSON. Errors: HTTP status + `{"error": "human readable message"}`.
All sizes are bytes (u64). All timestamps are RFC 3339 strings in UTC.
Every endpoint except `GET /api/health` and `POST /api/auth/login` requires a valid session cookie, else `401 {"error":"unauthorized"}`.
Mutating requests (POST/PUT/DELETE) must carry an `Origin` header whose host matches the `Host` header, else `403`.

**Sudo mode (v0.2).** Dangerous endpoints — marked **(sudo)** below — additionally require the session to have re-entered the password via `POST /api/auth/sudo` in the last 15 minutes, else `403 {"error":"sudo_required"}`. The client should prompt for the password, call `/api/auth/sudo`, and retry the original request.

**Audit (v0.2).** Every mutating request (any POST/PUT/PATCH/DELETE under `/api`, including failed logins and refused requests) is appended to the audit log: time, client IP, action, target, change/job id, plan hash and result. Read-only POSTs (previews) are not recorded. Bodies (passwords) are never logged.

## Auth

| Method | Path | Body | Response |
|---|---|---|---|
| GET | `/api/health` | – | `{"ok":true,"version":"0.2.0"}` (public) |
| POST | `/api/auth/login` | `{"password":"..."}` | `200 {"ok":true}` + `Set-Cookie: nc_session=<token>; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=43200`. Wrong password: `401 {"error":"wrong password"}`. More than 5 failures from one IP in 15 min: `429 {"error":"too many attempts, try again later"}` |
| POST | `/api/auth/logout` | – | `{"ok":true}` and clears cookie |
| GET | `/api/auth/me` | – | `{"ok":true,"sudo_until":"2026-11-13T10:15:00Z"}` — `sudo_until` is null when not in sudo mode |
| POST | `/api/auth/sudo` | `{"password":"..."}` | `200 {"ok":true,"sudo_until":"<rfc3339>"}`: the session is elevated for 15 min. Wrong password: `401 {"error":"wrong password"}`. Shares the login throttle (5 failures / 15 min per IP → `429`). |

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

`POST /api/services/{unit}/restart` **(sudo)** → `{"ok":true}` or error. `{unit}` must be in the known list (else 400).

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

`POST /api/databases/{engine}/{name}/backup` (`engine` = `mysql|postgres`, name must match `^[A-Za-z0-9_\-]{1,64}$` AND exist) → `{"job_id": "uuid"}`. Runs as a heavy job (one at a time) holding the lock `db:<engine>/<name>`, timeout 60 min; cancelling it kills the dump and removes the partial file.
Dump to `{backup_dir}/{engine}/{name}/{name}-{YYYYmmdd-HHMMSS}.sql.gz` (mysqldump `--single-transaction --quick --routines`, pg_dump), file mode 0600.

## Backups

`GET /api/backups` → array, newest first
```json
[{"engine": "mysql", "database": "shop_prod", "file": "mysql/shop_prod/shop_prod-20261113-030000.sql.gz", "size_bytes": 212000000, "created_at": "2026-11-13T03:00:00Z"}]
```
`GET /api/backups/download?file=<relative file>` → streams the file as `application/gzip` with `Content-Disposition: attachment`. The path must canonicalize to inside `backup_dir` (else 400).

## Jobs

Background work is a persistent job (SQLite `jobs` table + log file `{data_dir}/jobs/<id>.log`, mode 0600).

- **heavy** jobs (dumps, later installs/builds) run one at a time; light jobs run concurrently;
- named **locks** (`nginx`, `site:<id>`, `db:<engine>/<name>`, `apt`, …): two jobs naming the same lock never overlap;
- per-job **timeout**; **cancel** kills the job's process groups;
- jobs that were `queued`/`running` when the agent stopped are marked `interrupted` at startup;
- the 500 most recent finished jobs are kept.

`GET /api/jobs?limit=50` → newest first (`limit` 1–500, default 50; `log` is the last 16 KiB). `GET /api/jobs/{id}` (`log` is the last 256 KiB) →
```json
{"id": "uuid", "kind": "db_backup", "title": "Back up mysql/shop_prod",
 "status": "queued|running|ok|failed|cancelled|interrupted",
 "log": "[03:00:01] Started: Back up mysql/shop_prod\n…",
 "created_at": "2026-11-13T03:00:00Z", "started_at": "2026-11-13T03:00:01Z", "finished_at": null,
 "heavy": true, "locks": ["db:mysql/shop_prod"], "timeout_secs": 3600,
 "cancellable": true, "error": null, "change_id": null, "actor_ip": "203.0.113.5"}
```
- `started_at` is null while queued (in v0.1 it was the creation time; that is now `created_at`).
- `cancellable`: false for change jobs (applying a change must finish or auto-revert; they can still be cancelled while queued).
- `error`: failure / cancel reason. `change_id`: the change this job applies or rolls back. `actor_ip`: client IP, `"cli"`, or null (the agent itself).

`POST /api/jobs/{id}/cancel` **(sudo)** → `{"ok":true}`. `404` unknown job; `409` already finished, or running and not cancellable. A cancelled queued change job marks its change `cancelled`.

`GET /api/jobs/{id}/stream` → `text/event-stream` (SSE). Replays the existing log, then follows new lines; one event per line, then a final status event, then the stream ends:
```
event: log
data: [03:00:01] Started: Back up mysql/shop_prod

event: status
data: {"status":"ok","error":null}
```
`404` for an unknown job. Keep-alive comments are sent while idle.

## Changes

Every mutation of the server goes through the change pipeline: **plan → preview → snapshot → apply → verify → automatic revert on failure**, with one-click rollback later. A change only writes NexaCloud's own drop-in files (never vendor files), always as argv commands (never shell strings).

Apply runs as a job under the change's locks: (1) refuse if any file changed since the plan was made; (2) snapshot every touched file to `{data_dir}/snapshots/<change_id>/` (`manifest.json` + original bytes, mode, owner — or "did not exist"); (3) write files atomically (temp file + fsync + rename, mode/owner set); (4) run the commands (each must exit 0); (5) run the verify steps (command exits 0 / HTTP probe returns the expected status). Any failure in 3–5 restores the snapshot, runs the `on_revert` commands and marks the change `reverted`.

Change `status`: `pending` (job queued) · `applying` · `applied` · `failed` (failed before touching anything) · `reverted` (failed, snapshot restored automatically) · `revert_failed` (needs a person) · `rolled_back` (undone later) · `interrupted` (agent stopped mid-way; can be rolled back if its snapshot exists) · `cancelled`.

`GET /api/changes?limit=50` → newest first
```json
[{"id": "uuid", "kind": "nginx_hardening", "title": "Hide nginx version", "target": "nginx",
  "status": "applied", "created_at": "…", "finished_at": "…", "error": null,
  "job_id": "uuid", "actor_ip": "203.0.113.5", "rollback_of": null, "rolled_back_by": null,
  "plan_hash": "sha256 hex", "files_count": 1, "commands_count": 2, "can_rollback": true}]
```
`GET /api/changes/{id}` → the summary fields plus:
```json
{"note": null,
 "files": [{"path": "/etc/nginx/conf.d/zz-nexacloud.conf", "action": "create|modify|delete|permissions",
            "diff": "--- /dev/null\n+++ b/etc/nginx/conf.d/zz-nexacloud.conf\n@@ …\n+server_tokens off;\n",
            "mode": "0644", "owner": null, "old_sha256": null, "new_sha256": "…"}],
 "commands": [["nginx", "-t"], ["systemctl", "reload", "nginx"]],
 "verify": [{"kind": "command", "argv": ["systemctl", "is-active", "--quiet", "nginx"], "timeout_secs": 120}],
 "on_revert": [["systemctl", "reload", "nginx"]],
 "verify_results": [{"step": "systemctl is-active --quiet nginx", "ok": true, "detail": "exit 0"}]}
```
`owner` is `uid:gid` after the change (null for a new file created with the agent's default owner). `verify` items are `{"kind":"command","argv":[…],"timeout_secs":n}` or `{"kind":"http","url":"…","expect_status":200,"timeout_secs":n}`.

`POST /api/changes/{id}/rollback` **(sudo)**, body optional `{"force": false}` → `{"change_id": "<new rollback change>", "job_id": "…", "rollback_of": "<id>"}`. Restores the snapshot, runs `on_revert` and the original verify steps; recorded as a new change of kind `rollback` (with its own snapshot); the original becomes `rolled_back`. `404` unknown; `409` not rollbackable (not `applied`/`interrupted`), or **drift**: a file was edited after the change was applied (`{"error":"these files changed since the change was applied: … (roll back with force to overwrite them)"}`) — send `{"force": true}` to overwrite.

## Advisor (one-click fixes)

### nginx hardening — "Hide nginx version"
Writes `/etc/nginx/conf.d/zz-nexacloud.conf` with `server_tokens off;` — only directives **not already set at `http` level** in the effective config (`nginx -T`; commented lines don't count; our own drop-in is ignored). Commands: `nginx -t`, `systemctl reload nginx`; verify: `systemctl is-active --quiet nginx`; revert: restore + `systemctl reload nginx`. Lock: `nginx`.

`POST /api/advisor/nginx-hardening/preview` → preview (not audited):
```json
{"kind": "nginx_hardening", "title": "Hide nginx version", "target": "nginx",
 "nothing_to_do": false, "note": null, "plan_hash": "sha256 hex", "locks": ["nginx"],
 "files": [/* as in change detail */], "commands": [["nginx","-t"],["systemctl","reload","nginx"]],
 "verify": [/* … */], "on_revert": [["systemctl","reload","nginx"]]}
```
`nothing_to_do: true` with a `note` when already applied or set by hand. `409 {"error": …}` when nginx is missing or its current config fails `nginx -t`.

`POST /api/advisor/nginx-hardening/apply` **(sudo)** `{"plan_hash": "…"}` (optional) → `{"change_id", "job_id", "plan_hash"}`. The plan is rebuilt server-side; `409` when there is nothing to do, or when `plan_hash` no longer matches (the server changed since the preview).

## Audit

`GET /api/audit?limit=100` (1–1000) → newest first
```json
[{"id": 42, "ts": "2026-11-13T10:00:00Z", "ip": "203.0.113.5", "action": "change.apply", "target": "nginx",
  "change_id": "uuid", "job_id": "uuid", "plan_hash": "sha256 hex", "result": "ok"}]
```
Actions: `auth.login`, `auth.logout`, `auth.sudo`, `service.restart`, `db.backup`, `job.cancel`, `change.apply`, `change.rollback`, and outcomes recorded by the agent (`ip` null): `change.applied`, `change.reverted`, `change.failed`, `change.revert_failed`. Requests without a named action are recorded as `"<METHOD> <path>"`. `result` is `ok` or `"<http status> <error>"` / the failure reason. CLI actions have `ip: "cli"`. The table rejects UPDATE and DELETE.

## Test hooks (e2e only)
Only when the agent starts with `NEXACLOUD_TEST_HOOKS=1` (never in normal installs; otherwise `404`), both **(sudo)**: `POST /api/test-hooks/broken-change` (a fixed change whose `nginx -t` fails, to prove auto-revert) and `POST /api/test-hooks/sleep-job {"secs": 1..600, "heavy": bool}` (prints `tick N` every second).

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
data_dir = "/var/lib/nexacloud"          # v0.2: state, job logs, snapshots (0700)
state_db = "/var/lib/nexacloud/state.db" # v0.2: defaults to {data_dir}/state.db
```
Configs without `data_dir` / `state_db` keep working (defaults above). The agent creates `data_dir`, `data_dir/jobs` and `data_dir/snapshots` (0700) at startup.

## CLI (`nexacloud`)

- `nexacloud serve [--config PATH]` — run the agent (default config path `/etc/nexacloud/config.toml`).
- `nexacloud init [--config PATH]` — if the config does not exist: create it with a random 20-char admin password (argon2id hash stored), create a self-signed TLS cert/key (mode 0600) if missing, create `backup_dir` and `data_dir` (0700); print exactly one line `NEXACLOUD_ADMIN_PASSWORD=<password>` to stdout. If the config exists: print `config exists, keeping it` and change nothing.
- `nexacloud reset-password [--config PATH]` — generate a new password, update the hash, print `NEXACLOUD_ADMIN_PASSWORD=<password>`.
- `nexacloud rollback (--last | <change_id>) [--force] [--config PATH]` — undo an applied change from its snapshot; works with the service stopped (same state database and snapshots). `--last` picks the newest applied change that is not itself a rollback. Refuses drift unless `--force`. Recorded as a `rollback` change and in the audit log (`ip: "cli"`). Exit 1 on failure / nothing to roll back.
- `nexacloud doctor [--config PATH]` — readable report: config and TLS key permissions/owner, data dir mode, state database integrity + schema version, changes needing attention, `nginx -t`, disk space, service status. Exit 1 when any check fails (warnings don't fail).
- `nexacloud --version`.
