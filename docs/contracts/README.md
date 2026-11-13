# NexaCloud contracts: Phases 1–3

These files are the API and data-model contracts that builders implement in parallel.
They extend `docs/API.md`, which now documents v0.2 (Phase 0):
- jobs, cancel and SSE
- `/api/changes` and rollback
- sudo mode
- audit
- the first advisor fix, `nginx-hardening`

**`docs/API.md` is authoritative for every Phase 0 shape**, and this README has been
reconciled to it as of 2026-11-13. Phase 1–3 **additions** to Phase 0 shapes are marked
**[P0-ext]**. The Phase 0 owner must agree to them before they ship (see Q10).

| File | Area | Phases |
|---|---|---|
| [sites.md](sites.md) | Sites, domains, DNS, SSL, nginx templates and editor, drift, panel domain, adopt/import | 1 |
| [php-services.md](php-services.md) | PHP versions, per-site switch, extensions, ini, CLI default, service catalog, firewall | 1 (firewall/SSH confirm-or-revert is generalised in 3) |
| [backups.md](backups.md) | Destinations (rclone), plans, schedules, retention, restore, integrity, disk usage and cleanup | 1 |
| [advisor.md](advisor.md) | Advisor rules and fixes, dismiss/snooze, score, dashboard, metrics | 1 |
| [projects-deploys.md](projects-deploys.md) | Projects, wizards, deploy keys, webhooks, pipeline, releases, project detail tools | 2 (health prober: 1) |
| [database-manager.md](database-manager.md) | DB admin user, browse, edit, SQL console, users/grants, slow queries, import/export | 2 (PostgreSQL parity: 3) |
| [frankenphp.md](frankenphp.md) | FrankenPHP runtime per site | 3 |

Each endpoint below is tagged **P1**, **P2** or **P3**. Do not build a later-phase
endpoint early.

---

## 1. Shared conventions

### 1.1 Transport, auth and errors (v0.1, unchanged)
- JSON in and out. Sizes are bytes (u64). Timestamps are RFC 3339 UTC.
- Session cookie `nc_session` on every `/api` call except `GET /api/health` and
  `POST /api/auth/login`.
- POST, PUT and DELETE need an `Origin` whose host equals `Host`, else 403.
- **Exemption:** `/hooks/github/{site}` is outside `/api` and is public. It has no
  cookie and no Origin check, and is authenticated by HMAC. See projects-deploys.md §6.
  The SPA fallback must not serve `/hooks/*`.
- Behind the panel domain (sites.md §11), the agent trusts `X-Forwarded-For` and
  `X-Forwarded-Proto` **only** when the TCP peer is `127.0.0.1` or `::1`. Login
  throttling, audit IP and webhook rate limits all use that resolved client IP.

### 1.2 Error body
v0.1 is `{"error":"message"}`. Phases 1–3 add an optional machine-readable `code` and
optional `fields`/`details`, so existing clients keep working:
```json
{"error": "domain is not a valid hostname", "code": "invalid", "fields": {"domain": "must match HOSTNAME"}}
```

| HTTP | `code` | When |
|---|---|---|
| 400 | `invalid` | Validation failed. `fields` maps each input name to a message. |
| 400 | `confirmation_required` | A typed confirmation is missing or wrong. `details.expected` describes what to type, e.g. `"the database name"`. |
| 401 | `unauthorized` | No session, or bad webhook signature. |
| 403 | `forbidden_origin` | Origin check failed. |
| 403 | `sudo_required` | The endpoint needs sudo mode. Phase 0 sends `{"error":"sudo_required"}`, so clients must match on `error` **or** `code`. The client calls `POST /api/auth/sudo`, then retries. |
| 404 | `not_found` | |
| 409 | `stale_plan` | The plan changed since preview. Phase 0 returns a plain `409 {"error":…}`. [P0-ext]: `details.preview` carries the fresh preview. |
| 409 | `resource_busy` | Only for synchronous endpoints that cannot queue. `details.lock` and `details.job_id` name the blocker. |
| 409 | `unmanaged` | A managed-only operation was tried on an unmanaged site. |
| 409 | `drifted` | Files changed outside NexaCloud. `details.files` lists them. Re-apply or accept first. |
| 409 | `conflict` | Duplicate name, package conflict, port in use, and similar. |
| 409 | `nginx_broken` | `nginx -t` already fails **before** our change. `details.output` holds its output. |
| 422 | `precondition_failed` | Something must be done first, e.g. DNS not pointing or Redis not installed. `details.missing` lists it. |
| 424 | `not_installed` | A required package is missing, e.g. certbot, rclone, supervisor. |
| 429 | `rate_limited` | |
| 504 | `timeout` | A synchronous operation exceeded its limit, e.g. the SQL console. |

### 1.3 Ids and validation regexes
All regexes are anchored. They are written for Rust's `regex` crate, which has no
lookaround, so length limits are checked separately.

| Name | Regex / rule | Notes |
|---|---|---|
| `HOSTNAME` | `^([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]([a-z0-9-]{0,61}[a-z0-9])?$`, total ≤ 253 | Lowercased before matching. IDN is accepted only as punycode (`xn--`). No wildcards. |
| Site id | = primary `HOSTNAME` at creation. **Immutable.** | Same as the v0.1 `id`. To change the primary domain, create a new site plus a redirect (open question Q4). |
| `SLUG` | `^[a-z0-9][a-z0-9_]{0,23}$` | Per-site short name used in pool, socket, unit, cron and supervisor names. Derived from the first label of the domain (`-`→`_`). If that is taken, use the first two labels joined with `_`, then append `_2`, `_3`, and so on. Immutable. It has no dots because `/etc/cron.d` ignores names containing `.`. |
| Site user | `nc_<slug>` (≤ 27 chars; must match `^[a-z_][a-z0-9_-]{0,31}$`) | For new sites. Adopted sites keep their detected owner. |
| PHP version | `^(7\.4|8\.[0-5])$` | The supported set. |
| DB engine | `mysql` \| `postgres` | API names. The mockup's `pg` is UI-only. MariaDB reports as `mysql` with `flavor:"mariadb"`. |
| DB name (new) | `^[a-z0-9_]{1,64}$` (postgres ≤ 63) | Existing names must match v0.1 `^[A-Za-z0-9_\-]{1,64}$` **and** exist. |
| DB user | `^[a-z][a-z0-9_]{0,31}$` | |
| Job / change id | UUID v4 | From Phase 0. |
| Other rows | SQLite `INTEGER` ids, serialised as JSON numbers | Destinations, backup plans, deploys, rules state, and so on. |
| Port | integer 1024–65535 | |
| Unit names | `nc-<kind>-<slug>[-<name>]` | e.g. `nc-frankenphp-shop`, supervisor program `nc-shop-default`. |

### 1.4 Pagination
- Lists that can grow (changes, deploys, backup runs, audit, restore points) use cursor
  pagination: `?limit=50&cursor=<opaque>` returns `{"items":[...], "next_cursor": "..."|null}`.
  `limit` is 1–200, default 50.
- DB row browsing uses page-based paging: `?page=1&per_page=50` returns
  `{"rows":[...], "page":1, "per_page":50, "total": 1234|null, "total_exact": bool}`.
  See database-manager.md.
- Short, bounded lists (sites, PHP versions, services, destinations) return a bare
  array, as in v0.1.

### 1.5 Operation classes
**Every** mutating endpoint belongs to exactly one class. Each endpoint table in these
files has a `Class` column.

| Class | Shape | Used for |
|---|---|---|
| **C**: Change | `POST …/preview` with the operation body → `200 Preview` (§1.6). Not audited. Then `POST …/apply` with **the same body plus `"plan_hash"`** → `202 {"change_id","job_id","plan_hash"}`. The server **rebuilds the plan at apply**. If the hash differs, the response is `409` (the server changed since the preview). This follows the Phase 0 `nginx-hardening` endpoints. | Anything that writes files outside the panel's own state, or that restarts or reloads services. This covers nginx, PHP, systemd, supervisor, cron, `.env`, apt installs, DB server config and DB server objects. Snapshot, verify and auto-revert are handled by Phase 0. Rollback is `POST /api/changes/{id}/rollback` (sudo). |
| **J**: Job | `POST …` → `202 {"job_id"}` | Operational runs that do not change config: deploy, backup run, integrity check, scan, `artisan` run, cache clean, worker restart. They are audited and take locks. Rollback, where it exists, is a separate domain action, such as deploy rollback. |
| **D**: Data change | `POST …/preview` → `200 DataPlan` (the Preview fields plus `sql`, `params`, `before`, `affected_estimate`). Then `POST …/apply` with the same body plus `plan_hash` → `200 {"change_id","plan_hash","result":{…}}`, **synchronously**, with a 30 s limit. | Database row edits and SQL console writes. They are recorded as Phase 0 `changes` rows **[P0-ext: a change with no files and no job]**. Rollback runs a stored inverse statement and only succeeds if the rows are unchanged since. |
| **S**: State | `POST/PUT/DELETE …` → `200 {…}`, synchronous | Writes that only touch NexaCloud's own SQLite state: advisor dismiss and snooze, backup plan and destination records, deploy settings, project metadata. They are audited by the Phase 0 middleware. Bodies are never logged. They are **not** Changes and are not snapshotted (Q1). |

**Locks.** Each J and C row lists the Phase 0 named locks it takes. A job declares all
its locks up front and acquires them atomically, which is how the Phase 0 `LockManager`
works. **Heavy** jobs, one at a time, are marked `heavy`. A request whose locks are held
is **queued, not rejected.** Its job starts `queued` and the UI shows "waiting for
`nginx`".

| Lock | Guards |
|---|---|
| `site:<id>` | Everything under the site's base dir, its pool, its units, its `.env`. |
| `nginx` | Any file under `/etc/nginx`, `nginx -t`, reload. |
| `php:<ver>` | `/etc/php/<ver>/**`, and php<ver>-fpm reload and restart. |
| `apt` | dpkg/apt and repository files. **heavy** |
| `db:<engine>/<name>` | One database (dump, restore, import, drop, grants on it). Same spelling as Phase 0. |
| `db-server:<engine>` | Server config drop-ins, users, global grants, restarts. |
| `supervisor` | `/etc/supervisor/conf.d/nc-*`, `supervisorctl reread/update`. |
| `cron` | `/etc/cron.d/nexacloud-*`. |
| `certbot` | certbot runs and `/etc/letsencrypt`. |
| `backup-plan:<id>`, `dest:<id>` | One backup plan's run or retention pass; one remote's config, or a run writing to it. |
| `service:<unit>` | start, stop or restart of one unit. |
| `firewall`, `ssh`, `swap`, `sysctl`, `redis`, `fail2ban`, `logrotate`, `panel`, `node`, `composer`, `clamav`, `tailscale`, `frankenphp`, `disk-scan` | Self-explanatory. Each covers its own files and one-off runs. |

### 1.6 Preview (Phase 0 shape + [P0-ext] fields)
Every class C preview returns the Phase 0 preview object. Fields not in Phase 0 are
marked `ext`.
```json
{
  "kind": "site_create",
  "title": "Create site blog.example.com (Laravel, PHP 8.4)",
  "target": "site:blog.example.com",
  "nothing_to_do": false, "note": null,
  "plan_hash": "sha256 hex",
  "locks": ["site:blog.example.com", "nginx", "php:8.4"],
  "files": [
    {"path": "/etc/nginx/sites-available/blog.example.com.conf", "action": "create|modify|delete|permissions",
     "diff": "--- /dev/null\n+++ b/…\n@@ …", "mode": "0644", "owner": null, "old_sha256": null, "new_sha256": "…",
     "secret": false}
  ],
  "commands": [["useradd", "--system", "--user-group", "--home-dir", "/var/www/blog.example.com", "--no-create-home", "--shell", "/usr/sbin/nologin", "nc_blog"],
               ["nginx", "-t"], ["systemctl", "reload", "nginx"]],
  "verify": [{"kind": "command", "argv": ["nginx", "-t"], "timeout_secs": 30},
             {"kind": "http", "url": "http://blog.example.com/", "expect_status": 200, "timeout_secs": 10}],
  "on_revert": [["systemctl", "reload", "nginx"]],

  "tier": "safe|restart|lockout",
  "restarts": [{"unit": "nginx", "action": "reload"}],
  "steps": ["Create user nc_blog", "Write PHP-FPM pool", "Write nginx vhost", "Reload nginx", "Probe"],
  "heavy": false,
  "reversible": true,
  "warnings": ["DNS for blog.example.com does not point here yet; SSL will be issued when it does."],
  "data": {}
}
```
The fields after `on_revert` are the `ext` fields:
- `secret` (on file entries): the diff shows `KEY=<redacted:sha256-8>` instead of values.
  It is used for `.env`, credential files and htpasswd.
- `tier`: `safe` (reload only), `restart` (the UI names the units in `restarts`), or
  `lockout` (§1.8).
- `steps`: the human-readable plan, which drives "What will happen" and job progress.
- `reversible:false` marks plans whose effects outlive a snapshot restore, such as apt
  installs and file deletions. The UI must say so before apply.
- `data` carries area-specific extras, e.g. the adopt flow's `unmapped_directives`.

**Random material under rebuild-at-apply (rule R1).** Phase 0 rebuilds the plan at
apply and compares `plan_hash`. Any plan containing generated secrets or salts would
never match, so:
1. **Generation.** Secrets are generated **at apply**, never in preview. This covers
   passwords, `APP_KEY`, webhook secrets, deploy and crypt keys, and htpasswd salts.
2. **Placeholders.** The preview shows them as `<generated on apply>`.
3. **Hashing.** For `secret:true` files, `plan_hash` and `new_sha256` are computed over
   the **redacted** render, with every generated value replaced by its placeholder.
   They are not computed over the real bytes.
4. **Drift baseline.** After apply, the real `new_sha256` is recorded on the change, and
   drift detection uses it.
5. **Delivery.** The generated values reach the user only through the job `result`
   (one-time, §1.7).
6. **Operator-supplied secrets** are part of the request body, such as a basic-auth
   password or an S3 key. They are hashed as `sha256(secret)` inside the plan, so the
   hash still matches.

Endpoints this rule applies to:
- sites.md §4.1 and §8: basic auth.
- database-manager.md §2 and §6: admin users, create DB, password reset.
- projects-deploys.md §3 and §4: project create, deploy-key rotate.
- backups.md §2: destination keys and crypt.

**Phase 0 differences that builders must respect:**
- `verify` http items take a single `expect_status`. A "not 5xx" or "same class as
  before" probe, as used by adopt and the PHP switch, needs **[P0-ext]**
  `"expect":"not_5xx"|"same_class"` and `"resolve_to":"127.0.0.1"` (SNI). Until Phase 0
  supports these, run such probes as `command` verify steps that call `nexacloud probe …`
  (P1 CLI helper).
- **Write order.** Phase 0 writes files **before** running commands. Flows where a
  command must come first need **[P0-ext]** ordered stages, or are split into chained
  changes. Examples: site create (`useradd` before files owned by `nc_<slug>`) and PHP
  install (dpkg before `/etc/php/<v>/…` exists).
- **stdin.** Phase 0 commands are argv-only, and `GET /api/changes/{id}` returns them.
  Commands that need stdin (SQL with passwords, `openssl passwd -stdin`,
  `rclone obscure -`) need **[P0-ext]** `{"argv":[…],"stdin_ref":"secret:<name>"}`,
  shown in previews as `<stdin: redacted>`. Otherwise the agent must do the work
  in-process.
- **Command side effects.** These are files changed by a command rather than by the
  plan: UFW rules, `phpenmod` symlinks, `update-alternatives` links, apt state. They
  must be listed as **[P0-ext]** `snapshot_paths`, captured before the commands run.
  Each such change must also give an explicit `on_revert`, e.g.:
  - `userdel nc_<slug>`
  - `DROP DATABASE` / `DROP USER` (only if empty)
  - `swapoff /swapfile`
  - `ufw disable`
  - `update-alternatives --set php <old>`
- `commands` run **before** verify, and every one must exit 0. Multi-phase flows such as
  the PHP switch's "probe new pool, then swap nginx" therefore need **[P0-ext]**
  ordered stages, or are split into two chained changes. The second change is planned
  only after the first is applied, and both appear in Activity.

### 1.7 Job (Phase 0 shape + [P0-ext] `steps`, `result`)
Phase 0 job fields:
- `id`, `kind`, `title`
- `status`: `queued|running|ok|failed|cancelled|interrupted`
- `log`, `created_at`, `started_at`, `finished_at`
- `heavy`, `locks`, `timeout_secs`, `cancellable`
- `error`, `change_id`, `actor_ip`

Phases 1–3 add:
```json
{"steps": [
   {"name": "Add apt repository", "status": "ok", "started_at": "…", "finished_at": "…"},
   {"name": "Download packages", "status": "ok"},
   {"name": "Install php8.5-fpm + extensions", "status": "running"},
   {"name": "Create FPM pool", "status": "pending"},
   {"name": "Health check", "status": "pending"}],
 "result": null}
```
- Step `status` is one of `pending|running|ok|failed|skipped`.
- `result` is a kind-specific object, documented per job. It also carries one-time
  secrets, e.g. a generated DB password. A one-time secret is returned by
  `GET /api/jobs/{id}` **only** to the session that started the job, and only for 10
  minutes; after that it is replaced by `"<expired>"`.
- `cancellable` can become false during a run, e.g. once dpkg starts. Phase 0 already
  has the field.
- **Cancel** is `POST /api/jobs/{id}/cancel` and **needs sudo** (Phase 0).

### 1.8 Lockout tier: confirm-or-revert
Applies to UFW enable, any firewall change, any SSH change, and the panel IP allowlist.
1. Apply returns `confirm_deadline = now + 60 s`.
2. Before applying, the agent schedules an **independent** fallback:
   `systemd-run --unit=nc-revert-<change_id8> --on-active=90 /usr/local/bin/nexacloud rollback <change_id>`.
   This revert still runs if the agent dies.
3. The UI shows a countdown. It then calls `POST /api/changes/{id}/confirm` **P1** over
   a **new** connection (`fetch` with `cache:"no-store"` and `keepalive:false`), which
   proves the panel is still reachable. This is class S. The agent then runs
   `systemctl stop nc-revert-<id8>.timer`.
4. With no confirm by the deadline, the agent reverts by itself (the same rollback).
   Both paths are idempotent.

This is **[P0-ext]**. Phase 0 has no confirm endpoint. It is needed in Phase 1,
because the advisor's UFW fix uses it. Phase 3 reuses it for arbitrary firewall and SSH
edits. The rollback itself is the Phase 0 rollback, run without sudo by the revert
timer and CLI.

### 1.9 Sudo rule
Phase 0 sets the precedent:
- `POST /api/advisor/nginx-hardening/apply`, `POST /api/services/{unit}/restart`,
  `POST /api/jobs/{id}/cancel` and `POST /api/changes/{id}/rollback` all need sudo.
- Previews do not.

Phases 1–3 follow it:
- **Every class C and D `apply` needs sudo.**
- **Previews and reads do not,** except the reveal endpoints.

**The `Sudo` column has one meaning.** On a `preview` / `apply` pair row it marks sudo
needed **beyond** the baseline: `✓` means the **preview** needs sudo too, because it
returns or reveals secrets or raw content. `–` means only the apply needs it. On J and S
rows, `✓` means the endpoint needs sudo. It is set when **any** of these is true:
1. It destroys or overwrites data irreversibly: drop DB, user or site; restore-replace;
   SQL import; delete backups or destinations.
2. It reveals or sets a secret: reveal a `.env` value, set or reveal destination
   credentials or a crypt password, download a recovery kit, store a GitHub PAT, rotate
   the webhook secret.
3. It changes panel access: TOTP, the panel IP allowlist.
4. It stops or restarts a service, matching Phase 0's restart.

J endpoints that only run project or backup work need **no** sudo: deploy, redeploy,
backup run, scans, artisan allowlist (except `✓` commands), worker restart.

`✓*` marks sudo that applies only under a stated condition, e.g. only for a raw edit.

**Sudo on applies (Q22).** The Phase 0 precedent makes every apply sudo. The
alternative is to require sudo only for applies that match rules 1–3 plus lockout tier,
which reduces prompts, e.g. on toggling gzip. These contracts assume the precedent. If
Phase 0 relaxes it, rules 1–3 plus lockout tier and raw editors keep sudo.

### 1.10 Typed confirmations
Destructive bodies carry `"confirm": "<string>"`, which must equal the object name, e.g.
`crm_prod`. If it does not, the response is `400 confirmation_required`. This is
separate from sudo. Some endpoints need both.

### 1.11 SSE (Phase 0 shape + [P0-ext])
`GET /api/jobs/{id}/stream` (Phase 0) replays the log, then follows it:
- `event: log` carries **one plain-text line** as `data`.
- The final `event: status` carries `data: {"status":"ok","error":null}`.
- Keep-alive comments are sent while idle.

**[P0-ext]** for P1, additive, so old clients ignore it:
```
event: step
data: {"index":2,"name":"Install php8.5-fpm + extensions","status":"running"}
```
It also adds `id: <line number>` on `log` events, so `Last-Event-ID` can resume.

Log tails (projects-deploys.md §9.6) use the same framing with `log` events only. The
agent sends `X-Accel-Buffering: no` so nginx (panel domain) does not buffer.

The Install mockup's "HTTPS + WebSocket" label should read "HTTPS + SSE". There is no
WebSocket in the plan.

### 1.12 Secrets handling (all areas)
- **Never put secrets in argv**, because `/proc/<pid>/cmdline` is world-readable. This
  covers DB passwords, PATs, rclone credentials, Tailscale auth keys and htpasswd
  passwords. Pass them by one of these routes:
  - **stdin.** e.g. SQL with `IDENTIFIED BY` piped to `mysql`, `openssl passwd -6 -stdin`,
    `rclone obscure -`.
  - **A defaults file.** `--defaults-extra-file=/run/nexacloud/tmp/<uuid>.cnf`, mode 0600,
    deleted after use. This applies to the long-lived file under
    `/etc/nexacloud/secrets/` too.
  - **The environment of the child process.** e.g. `PGPASSWORD`, `GIT_SSH_COMMAND`
    pointing at a key path.
- Long-lived secrets live in `/etc/nexacloud/secrets/` (dir 0700 root, files 0600):
  - `mysql-admin.cnf`
  - `pg-admin.pgpass`
  - `rclone/<dest_id>.json`
  - `github/<site_id>.webhook`
  - `github/pat` (optional)
  - `backup-crypt/<dest_id>.json`

  SQLite stores only references. These files are included in the server-config backup,
  which is crypt-only (backups.md §4).
- `/run/nexacloud/` (tmpfs, 0700) holds per-run renders, such as an rclone config. They
  are deleted when the job ends.

### 1.13 Managed config files ("drop-ins only")
Each NexaCloud-owned file is **rendered as a whole** from SQLite state
(`managed_settings(file, section, key, value, source_rule)`). A change therefore always
diffs one full file. Vendor files are never edited.

| File | Owner area |
|---|---|
| `/etc/php/<v>/{fpm,cli}/conf.d/99-nexacloud.ini` | php-services, advisor |
| `/etc/php/<v>/fpm/pool.d/<slug>.conf` | sites, php-services |
| `/etc/mysql/mysql.conf.d/zz-nexacloud.cnf` (MariaDB: `/etc/mysql/mariadb.conf.d/zz-nexacloud.cnf`) | advisor, database-manager |
| `/etc/postgresql/<v>/main/conf.d/zz-nexacloud.conf` | advisor (P3 parity) |
| `/etc/nginx/conf.d/zz-nexacloud.conf` (http context) | sites, advisor |
| `/etc/nginx/snippets/nexacloud-*.conf` | sites |
| `/etc/systemd/system/redis-server.service.d/zz-nexacloud.conf` + `/etc/nexacloud/redis.conf` | php-services, advisor |
| `/etc/sysctl.d/99-nexacloud.conf`, `/etc/systemd/system/swapfile.swap` | advisor |
| `/etc/fail2ban/jail.d/zz-nexacloud.local` | php-services, advisor |
| `/etc/apt/apt.conf.d/21nexacloud-auto-upgrades` | advisor |
| `/etc/logrotate.d/nexacloud-<slug>` | advisor, projects |
| `/etc/supervisor/conf.d/nc-<slug>-<name>.conf` | projects |
| `/etc/cron.d/nexacloud-<slug>` | projects |
| `/etc/ssh/sshd_config.d/99-nexacloud.conf` | php-services §8 (P3) |

### 1.14 Server facts used by examples
The examples assume:
- Ubuntu 22.04 or 24.04
- 2 vCPU, 8 GB RAM
- nginx 1.18 on 22.04, 1.24 on 24.04, or ≥ 1.25.1 from nginx.org (template syntax differs; see sites.md §7.1)
- MySQL 8.0 from the distro, or 8.4 from the MySQL repo

---

## 2. Endpoint index
This is the complete list. Each file holds the bodies, sudo and locks. `pv|ap` means
the pair `…/preview` and `…/apply`.

| Area (file) | Endpoints (phase) |
|---|---|
| Phase 0 / v0.1 (docs/API.md, unchanged) | `GET /api/health`, `/api/auth/*`, `GET /api/overview`, `GET /api/system`, `GET /api/services`, `GET /api/php`, `GET /api/sites`, `GET /api/databases`, `POST /api/databases/{engine}/{name}/backup`, `GET /api/backups`, `GET /api/backups/download`, `GET /api/jobs`, `GET /api/jobs/{id}`, `GET /api/jobs/{id}/stream`, `POST /api/jobs/{id}/cancel`, `GET /api/changes`, `GET /api/changes/{id}`, `POST /api/changes/{id}/rollback`, `POST /api/advisor/nginx-hardening/{preview,apply}`, `GET /api/audit`, `GET /api/security` |
| Shared (README) | `POST /api/changes/{id}/confirm` (P1, [P0-ext]); `PUT /api/settings` (P1, S: `admin_email`, `bandwidth_quota_bytes`, `provider_panel_url`) |
| Sites (sites.md) | P1: `GET /api/sites/{id}`, `GET /api/domains`, `POST /api/dns/check`, `GET /api/import/scan`, `POST /api/import/pv\|ap`, `DELETE /api/import/{id}`, `POST /api/sites/pv\|ap`, `POST /api/sites/{id}/adopt/pv\|ap`, `…/delete/pv\|ap`, `…/domains/pv\|ap`, `…/basic-auth/pv\|ap`, `…/ssl/pv\|ap`, `…/options/pv\|ap`, `…/custom-conf/pv\|ap`, `…/raw/pv\|ap`, `…/reapply/pv\|ap`, `GET /api/sites/{id}/nginx`, `POST /api/sites/{id}/nginx/test`, `GET /api/sites/{id}/drift`, `POST /api/sites/{id}/drift/accept`, `GET /api/panel/domain`, `POST /api/panel/domain/pv\|ap`. P3: `POST /api/panel/allowlist/pv\|ap` |
| PHP & services (php-services.md) | P1: `GET /api/php` (extended), `GET /api/php/sites`, `POST /api/php/install/pv\|ap`, `POST /api/php/{ver}/uninstall/pv\|ap`, `POST /api/php/cli-default/pv\|ap`, `GET /api/php/{ver}/extensions`, `POST /api/php/{ver}/extensions/pv\|ap`, `GET /api/php/{ver}/ini`, `POST /api/php/{ver}/ini/pv\|ap`, `POST /api/sites/{id}/php/scan`, `POST /api/sites/{id}/php/pv\|ap`, `GET /api/catalog`, `POST /api/catalog/{id}/install/pv\|ap`, `POST /api/catalog/{id}/uninstall/pv\|ap`, `POST /api/services/{unit}/{start,stop,restart}`, `POST /api/redis/flush`, `GET /api/firewall`, `POST /api/firewall/pv\|ap`. P3: `POST /api/ssh/pv\|ap`, extended firewall rules |
| Backups (backups.md) | P1: `GET/POST /api/backup/destinations`, `PUT/DELETE /api/backup/destinations/{id}`, `POST /api/backup/destinations/{id}/test`, `GET /api/backup/destinations/{id}/recovery-kit`, `POST /api/backup/destinations/hostkey`, `GET /api/tailscale`, `POST /api/tailscale/up`, `GET/POST /api/backup/plans`, `PUT/DELETE /api/backup/plans/{id}`, `POST /api/backup/plans/{id}/run`, `POST /api/backup/run-all`, `GET /api/backup/runs`, `GET /api/backup/points`, `POST /api/backup/points/{id}/download`, `POST /api/backup/points/{id}/verify`, `POST /api/backup/points/{id}/restore/pv\|ap`, `POST /api/backup/restores/{rid}/swap/pv\|ap`, `POST /api/backup/restores/{rid}/discard`, `GET /api/disk`, `POST /api/disk/scan`, `POST /api/disk/cleanup/pv\|ap` |
| Advisor & dashboard (advisor.md) | P1: `GET /api/advisor`, `GET /api/advisor/budget`, `POST /api/advisor/scan`, `POST /api/advisor/{rule}/pv\|ap`, `POST /api/advisor/apply-safe/pv\|ap`, `POST /api/advisor/{rule}/{dismiss,snooze,restore}`, `GET /api/dashboard`, `GET /api/metrics` |
| Projects & deploys (projects-deploys.md) | P1: `GET /api/setup/preflight`, `GET /api/sites/{id}/health`, `GET /api/sites/{id}/response-times`. P2: `GET /api/projects`, `GET /api/projects/{id}`, `POST /api/projects/pv\|ap`, `PUT /api/projects/{id}/settings`, `GET /api/projects/{id}/deploy-key`, `POST /api/projects/{id}/deploy-key/rotate/pv\|ap`, `POST /api/projects/{id}/github/{verify,bootstrap}`, `POST /api/projects/{id}/webhook-secret/rotate`, `PUT/DELETE /api/github/pat`, `GET /api/github/status`, `GET /api/github/repos`, `GET /api/github/repos/{owner}/{repo}/branches`, `GET /api/import/github-matches`, `POST /api/projects/{id}/deploys`, `GET /api/deploys`, `GET /api/deploys/{id}`, `POST /api/deploys/{id}/{redeploy,rollback}`, `GET /api/projects/{id}/releases`, `POST /api/projects/{id}/releases/{release}/activate`, `GET/PUT /api/projects/{id}/deploy-settings`, `GET /api/projects/{id}/deploy-settings/defaults`, `GET /api/projects/{id}/actions-workflow`, `POST /hooks/github/{site}` (public), `POST /api/projects/{id}/composer-audit`, `POST /api/projects/{id}/artisan`, `GET /api/projects/{id}/workers`, `POST /api/projects/{id}/workers/pv\|ap`, `POST /api/projects/{id}/workers/restart`, `POST /api/projects/{id}/workers/{name}/restart`, `GET /api/projects/{id}/scheduler`, `POST /api/projects/{id}/scheduler/pv\|ap`, `GET /api/projects/{id}/env`, `POST /api/projects/{id}/env/reveal`, `POST /api/projects/{id}/env/pv\|ap`, `GET /api/sites/{id}/logs`, `GET /api/sites/{id}/logs/{log}`, `GET /api/sites/{id}/logs/{log}/stream`, `GET /api/sites/{id}/logs/{log}/download`, `GET /api/sites/{id}/errors`, `POST /api/sites/{id}/maintenance/pv\|ap` |
| DB manager (database-manager.md) | P2 for MySQL; the same paths for `postgres` are P3: `GET /api/db/{engine}/admin-user`, `POST /api/db/{engine}/admin-user/pv\|ap`, `GET /api/db/{engine}/overview`, `GET /api/db/{engine}/databases/{db}/tables`, `GET …/tables/{table}`, `GET …/tables/{table}/rows`, `POST …/tables/{table}/rows/pv\|ap`, `POST /api/db/{engine}/query`, `POST /api/db/{engine}/query/pv\|ap`, `POST /api/db/{engine}/databases/pv\|ap`, `POST /api/db/{engine}/databases/{db}/drop/pv\|ap`, `GET /api/db/{engine}/users`, `POST /api/db/{engine}/users/pv\|ap`, `POST /api/db/{engine}/users/{user}/{grants,password,drop}/pv\|ap`, `GET /api/db/{engine}/slow-queries`, `POST /api/db/{engine}/databases/{db}/export`, `POST /api/db/{engine}/databases/{db}/import/upload`, `POST /api/db/{engine}/databases/{db}/import/pv\|ap`, `GET /api/db/connection-info` |
| FrankenPHP (frankenphp.md) | P3: `GET /api/frankenphp`, `POST /api/frankenphp/install/pv\|ap`, `POST /api/sites/{id}/runtime/preflight`, `POST /api/sites/{id}/runtime/pv\|ap`, `POST /api/sites/{id}/runtime/limits/pv\|ap` |

## 3. Design-screen → endpoint matrix
`—` in the endpoint column means the element is client-only or comes from data already
listed. **OUT** means out of scope per the roadmap's "Not planned" list, or not in any
phase. Builders must not implement it, and the UI should hide it or link out. **Q#**
refers to §4.

### 3.1 Shell (every screen)
| Element | Endpoint | Phase |
|---|---|---|
| Server name / OS line ("nexadev.io Ubuntu 24.04 · KVM 2") | `GET /api/system` (+ `virt` from `systemd-detect-virt`, advisor.md §5) | P1 |
| "Agent connected / heartbeat 4s ago / agent 0.1.0 · Rust" | `GET /api/health`, polled every 10 s | v0.1 |
| Nav badge "Projects 2" | `GET /api/dashboard` → `nav_badges` | P1 |
| ⌘K search | — client-side over loaded lists | — |
| "New project" button | → Projects wizard | P2 |

### 3.2 Dashboard (Main)
| Element | Endpoint | Phase |
|---|---|---|
| "N things need your attention" list (level, text, action link) | `GET /api/dashboard` → `attention[]` (advisor top issues + v0.1 alerts) | P1 |
| Action "Upgrade" (PHP EOL) | link → PHP page, `POST /api/sites/{id}/php/preview` | P1 |
| Action "Fix" (firewall) | `POST /api/advisor/ufw-inactive/preview` (lockout) | P1 |
| "Hostinger firewall has 0 rules" | **OUT** (the hPanel cloud firewall has no API). Replaced by the UFW rule. | — |
| Action "Restart workers" | `POST /api/projects/{id}/workers/restart` | P2 |
| Action "Install" (malware scanner) | `POST /api/catalog/clamav/install/preview` | P1 |
| CPU % + 24 h sparkline, Memory %, Disk used/total | `GET /api/dashboard` → `system`, `GET /api/metrics?range=24h` | P1 |
| "See what uses the space" | → Backups & Storage disk card (`GET /api/disk`) | P1 |
| Bandwidth this month, in/out 24 h | `GET /api/dashboard` → `bandwidth` (from `/proc/net/dev` deltas, advisor.md §5) | P1 |
| Project health table: name, repo·branch, stack (Laravel 12 / PHP), status, p95, uptime 30 d, last deploy, Open | `GET /api/dashboard` → `projects[]` (health prober is P1; repo and deploy fields are null until P2) | P1/P2 |
| Services card: name, meta (version, counts), state | `GET /api/dashboard` → `services[]` | P1 |
| Recent deployments | `GET /api/deploys?limit=4` | P2 |
| Backups card: last run, next run, retention, off-site, "Back up now" | `GET /api/dashboard` → `backups`; `POST /api/backup/run-all` | P1 |
| "VPS snapshots 2 (Hostinger)" | **OUT** (no API). Optional static link setting `provider_panel_url` (S). | — |
| Security score N/100 + list | `GET /api/dashboard` → `security` | P1 |
| "Apply recommended hardening" | `POST /api/advisor/apply-safe/preview` → apply. Only **safe**-tier security rules; the others are listed for one-by-one review. | P1 |

### 3.3 Projects
| Element | Endpoint | Phase |
|---|---|---|
| Filter chips All / Laravel / Plain PHP with counts | `GET /api/projects` (`kind`, counts client-side) | P2 |
| Card: initial, name, repo·branch, status, fw+version, PHP, DB, extra, p95, uptime, disk; Open / Deploy / Logs | `GET /api/projects`; `POST /api/projects/{id}/deploys`; logs → project detail | P2 |
| Wizard 1: type Laravel / Plain PHP / Static | `POST /api/projects/preview` → `kind` | P2 |
| Wizard 2: repository dropdown, "Connect another account…" | `GET /api/github/repos` (needs a stored PAT, else free-text URL); "connect account" = `PUT /api/github/pat`. **GitHub App: Q2.** | P2 |
| Branch, PHP version (recommended flag), domain, web root | `GET /api/php` (installed + `recommended`), `GET /api/github/repos/{owner}/{repo}/branches` | P2 |
| Wizard 3: database MySQL / Postgres / None | `db` field in the project create body | P2 |
| Wizard 4: SSL, nightly DB backup, queue worker (2), scheduler, npm build, composer install | `extras` in the body | P2 |
| "What will happen" plan | `POST /api/projects/preview` → `steps` | P2 |
| "Create project" | `POST /api/projects/apply` | P2 |

### 3.4 Project detail
| Element | Endpoint | Phase |
|---|---|---|
| Breadcrumb, name, status pill, tags (framework, PHP, DB, repo@branch, path) | `GET /api/projects/{id}` | P2 |
| Visit site | — (`url`) | — |
| "Maintenance mode: Off" toggle | `POST /api/sites/{id}/maintenance/{preview,apply}` | P2 |
| Deploy now | `POST /api/projects/{id}/deploys` | P2 |
| Tabs Overview / Environment / Logs | — | — |
| Health checks grid (HTTP, SSL days, queue pending, scheduler, error rate, storage/logs size + "Rotate logs", PHP version, Composer outdated, uploads size) | `GET /api/sites/{id}/health`; "Rotate logs" → `POST /api/disk/cleanup/preview` scoped to the site; Composer outdated → `POST /api/projects/{id}/composer-audit` (J) | P1 prober / P2 rest |
| Response time 24 h chart p50/p95 + note | `GET /api/sites/{id}/response-times?range=24h`. The note is client text from `health`. | P1 |
| "PHP 8.1 is end-of-life… 2 deprecations… Review & upgrade" | `POST /api/sites/{id}/php/scan` (J) → result; upgrade = `POST /api/sites/{id}/php/preview` | P1 |
| PHP project settings: web root, front controller, composer on deploy, keep uploads/ | `PUT /api/projects/{id}/settings` (S). Web root or front controller change → `POST /api/sites/{id}/options/preview` (C). | P2 |
| Artisan buttons + free input | `POST /api/projects/{id}/artisan` (J, allowlist) | P2 |
| Queue workers table (name, queue, procs, pending, failed 24 h, status, uptime) + Add / Restart all / Restart | `GET /api/projects/{id}/workers`; `POST …/workers/{preview,apply}`; `POST …/workers/restart`; `POST …/workers/{name}/restart` | P2 |
| Scheduler table (command, runs, last run, result) | `GET /api/projects/{id}/scheduler`; enable/disable `POST …/scheduler/{preview,apply}` | P2 |
| Latest errors (level, time, count, message, file) | `GET /api/sites/{id}/errors?since=24h` | P2 |
| .env table (key, masked value, Edit/Reveal), Edit as text, Save & reload | `GET /api/projects/{id}/env`; `POST …/env/reveal` (sudo); `POST …/env/{preview,apply}`. "Encrypted at rest" copy: **Q6**. | P2 |
| Log selector (laravel.log, nginx access/error, php-fpm, worker), Live tail, Download | `GET /api/sites/{id}/logs`, `…/logs/{log}?lines=`, `…/logs/{log}/stream` (SSE), `…/logs/{log}/download` | P2 |

### 3.5 Domains & SSL
| Element | Endpoint | Phase |
|---|---|---|
| "Re-check all DNS" | `POST /api/dns/check` (all) | P1 |
| "Add domain" | `POST /api/sites/preview` (new site) or `POST /api/sites/{id}/domains/preview` (alias) | P1 |
| DNS instructions card (A @ / A * → server IPv4), Copy records | `GET /api/domains` → `server_ips`. The `*` record is a DNS hint only; **wildcard certs are OUT**. | P1 |
| Domain table: name, kind (Primary/Redirect/Subdomain/Proxy/Alias), serves, DNS status, SSL status/days, PHP, Nginx button, ··· menu | `GET /api/domains`; ··· menu = issue/renew SSL, remove domain, delete site, adopt | P1 |
| Add subdomain: name + zone suffix, "Points to" Project / Redirect / Proxy | `POST /api/sites/preview` with `kind` = laravel\|php\|static / `redirect` / `proxy` | P1 |
| Points to Project: existing project + branch ("shop (develop)") or new empty | P1 creates an empty site. Branch staging copy (`clone_from`) is P2. | P1/P2 |
| Redirect to + code 301/302 | `kind:"redirect"`, `redirect.{to,code,preserve_path}` | P1 |
| Proxy: local port | `kind:"proxy"`, `proxy.{port,websocket}` | P1 |
| Issue SSL certificate checkbox | `ssl:true` | P1 |
| Password-protect (basic auth) | `basic_auth:{username,password}` (sudo, secret) | P1 |
| Create subdomain | `POST /api/sites/apply` | P1 |
| Nginx drawer: path, Visual/Raw toggle | `GET /api/sites/{id}/nginx` | P1 |
| Visual toggles (Force HTTPS, HTTP/2, Gzip, Cache static, Security headers, Rate limit /login, Block hidden files), Max upload 64/128/256M | `POST /api/sites/{id}/options/{preview,apply}` | P1 |
| Raw config editor | managed: `custom-conf/{preview,apply}`; unmanaged/detached: `raw/{preview,apply}` (sudo) | P1 |
| "nginx -t: syntax ok" | `POST /api/sites/{id}/nginx/test` (dry-run in a temp tree) | P1 |
| "Previous version saved. Roll back any time." | Phase 0 `GET /api/changes` + `POST /api/changes/{id}/rollback`; the `?target=site:{id}` filter is [P0-ext] | P0/P1 |
| Test config / Save & reload | `…/nginx/test`; `…/{options,custom-conf,raw}/apply` | P1 |

### 3.6 Deployments (GitHub CI/CD)
| Element | Endpoint | Phase |
|---|---|---|
| "GitHub App · nexadev · 7 repos" header | `GET /api/github/status` (PAT owner + repo count). **Q2.** | P2 |
| "Deploy a project" | `POST /api/projects/{id}/deploys` | P2 |
| Recent runs list (state, project #n, sha·msg, when) | `GET /api/deploys` | P2 |
| Run detail (branch, sha, msg, author) + Redeploy + Roll back to previous | `GET /api/deploys/{id}`; `POST /api/deploys/{id}/redeploy`; `POST /api/deploys/{id}/rollback` | P2 |
| Steps (icon, name, cmd, result, duration), log, note | `GET /api/deploys/{id}` → `steps[]`, `log_tail`, `note`; live: job SSE | P2 |
| Pipeline project selector | — | — |
| Deploy when: push to branch / new tag v* / manual | `PUT /api/projects/{id}/deploy-settings` → `trigger` | P2 |
| Build on: this server / GitHub Actions | `deploy-settings.source` = `webhook` \| `actions` (projects-deploys.md §7) | P2 |
| Toggles: zero-downtime releases, stop if tests fail, back up DB before migrate, notify on failure | `deploy-settings.{mode,run_tests,backup_before_migrate}`. Notify: **OUT** in P2 (Q8). The toggle is hidden. | P2 |
| Deploy script editor + Reset to Laravel default | `deploy-settings.build_hook` (sudo to change); `GET /api/projects/{id}/deploy-settings/defaults` | P2 |
| GitHub Actions YAML + Copy file | `GET /api/projects/{id}/actions-workflow` | P2 |
| "The deploy token is stored as a repo secret automatically" | Only with a PAT: `POST /api/projects/{id}/github/bootstrap` (secrets scope). Otherwise the UI shows the manual instructions. | P2 |
| Releases on disk + Activate | `GET /api/projects/{id}/releases`; `POST …/releases/{rel}/activate` | P2 |

### 3.7 PHP & Services (Runtimes)
| Element | Endpoint | Phase |
|---|---|---|
| Header "Packages from ondrej/php, MySQL & PGDG apt repositories" | `GET /api/catalog` → `repos[]` | P1 |
| Install progress card (title, step N of M, elapsed, Show output, step list) | Job + SSE (`steps`) | P1 |
| PHP version cards (version, state, support, sites, CLI default) + "Make CLI default" / "Cancel install" | `GET /api/php` (extended); `POST /api/php/cli-default/{preview,apply}`; `POST /api/jobs/{id}/cancel` (P0) | P1 |
| (Install a new version) | `POST /api/php/install/{preview,apply}` | P1 |
| Per-site PHP table (now, switch-to select, compatibility, Apply/Scan/Review & switch) | `GET /api/php/sites`; `POST /api/sites/{id}/php/scan`; `POST /api/sites/{id}/php/{preview,apply}` | P1 |
| Extensions grid per version (version tabs) | `GET /api/php/{ver}/extensions`; `POST …/extensions/{preview,apply}` | P1 |
| php.ini key/values + "Save & reload FPM" | `GET /api/php/{ver}/ini`; `POST …/ini/{preview,apply}` | P1 |
| Services catalog cards (MySQL, PostgreSQL, Redis, MariaDB, Node, Composer, Supervisor, UFW, Fail2ban, ClamAV) with Manage/Install/Restart/Update | `GET /api/catalog`; `POST /api/catalog/{id}/install/{preview,apply}`; `POST /api/services/{unit}/restart` | P1 |
| MongoDB, Meilisearch cards | **OUT** (not in roadmap). The catalog returns `available:false`. Q9. | — |
| PostgreSQL "Upgrade" (17→18) | **OUT** (major upgrades need `pg_upgradecluster`; not planned) | — |
| Redis "Flush" | `POST /api/redis/flush` (sudo + typed confirm) | P1 |
| MariaDB "can run beside MySQL on port 3307" | **Not possible with distro packages**: `mariadb-server` and `mysql-server` conflict. The catalog returns `conflicts_with:["mysql"]`. Q3. | — |
| UFW "Enable with defaults" | `POST /api/firewall/{preview,apply}` (lockout) | P1 |
| Composer "Self-updates weekly" / "Update" | `POST /api/catalog/composer/install/preview` (re-install = update) + timer | P1 |

### 3.8 Databases
| Element | Endpoint | Phase |
|---|---|---|
| Engine tabs MySQL / PostgreSQL | `GET /api/databases` (v0.1) + `GET /api/db/{engine}/overview` | P2 (PG P3) |
| Stats: databases, total size, connections now/max, buffer-pool / cache hit rate | `GET /api/db/{engine}/overview` → `stats` | P2 |
| "Import .sql" | `POST /api/db/{engine}/databases/{db}/import/upload` → preview → apply | P2 |
| "Query console" | `POST /api/db/{engine}/query` (read) / `query/{preview,apply}` (write) | P2 |
| DB table: name, used by project, size, tables, users, last backup; Back up / Browse / ··· | `GET /api/db/{engine}/overview` → `databases[]`; v0.1 backup endpoint; browse = tables endpoint; ··· = export, drop | P2 |
| Create database form (name, user, generated password + Copy/New, connect to project) | `POST /api/db/{engine}/databases/{preview,apply}`; password is generated at apply and returned once in the job `result` | P2 |
| Users table (user, host, access, privileges, Reset password) + Add user | `GET /api/db/{engine}/users`; `POST …/users/{preview,apply}`; `POST …/users/{u}/password/{preview,apply}` | P2 |
| Slow queries 24 h (sql, db, time, count) | `GET /api/db/{engine}/slow-queries?range=24h` | P2 |
| Remote access "Local only" + "Copy SSH tunnel settings" | `GET /api/db/connection-info`. Opening remote access is **OUT**. | P2 |
| (Browse view: tables, columns, indexes, rows, edit) | database-manager.md §3–4. No mockup screen; follow the existing panel style. | P2 |

### 3.9 Backups & Storage
| Element | Endpoint | Phase |
|---|---|---|
| "New backup job" | `POST /api/backup/plans` (S) | P1 |
| "Back up everything now" | `POST /api/backup/run-all` | P1 |
| Disk bar + breakdown parts + growth "≈ 0.4 GB/week · full in ~3 years" | `GET /api/disk` | P1 |
| Free up space: rotate logs, prune releases, clear caches, move local backups off-site (sizes) | `GET /api/disk` → `cleanup[]`; `POST /api/disk/cleanup/{preview,apply}` | P1 (prune releases P2) |
| Backup jobs table (name, includes, schedule, goes to, keeps, last run) + Run now / Edit | `GET /api/backup/plans`; `POST …/{id}/run`; `PUT …/{id}` | P1 |
| "Before every deploy" job | built-in plan `trigger:"pre_deploy"` | P2 |
| Restore points tabs (per DB / all DBs / project files) + list (when, kind, size, where) + Download / Restore… | `GET /api/backup/points?target=…`; `POST /api/backup/points/{id}/download` | P1 |
| Restore dialog: into new copy vs replace; typed confirm; Start restore | `POST /api/backup/points/{id}/restore/{preview,apply}` (`mode: copy\|replace`) | P1 |
| Destinations list (Local, S3, B2, SFTP) + Browse/Settings/Connect | `GET/POST/PUT /api/backup/destinations`, `POST …/{id}/test` | P1 |
| "Hostinger snapshots / Open hPanel" | **OUT** (link via `provider_panel_url`) | — |
| "Encrypted (AES-256)… keep a copy of the recovery key" | crypt; `GET /api/backup/destinations/{id}/recovery-kit` (sudo). The copy must say "encrypted (rclone crypt: XSalsa20-Poly1305)". Q6. | P1 |
| Retention "14 daily, 8 weekly" | Per-plan `retention`. Default 7d/4w/3m; the mockup values are just another setting. | P1 |

### 3.10 Install & Import
| Element | Endpoint | Phase |
|---|---|---|
| Step 1 install command, install log, "waiting for agent to call home" | **Installer, not API.** The page polls `GET /api/health`. The "one-time setup code at /setup" flow is **Q5**; v0.1 prints the admin password. | — |
| Pre-flight check (OS, CPU/mem, disk, existing nginx, other panel, port 8443) | `GET /api/setup/preflight` (projects-deploys.md §10) | P1 |
| "update: nexacloud self-update / uninstall --keep-sites" | CLI only. `self-update` does not exist (install.sh re-run is the upgrade): **Q5**. | — |
| Step 2: panel address | `POST /api/panel/domain/{preview,apply}` | P1 |
| Admin email | `PUT /api/settings` → `admin_email` (S; also the Let's Encrypt account email) | P1 |
| Require TOTP | Not built yet: TOTP is a Phase 0 TODO (ROADMAP). The checkbox stays hidden until it ships. | P0 TODO |
| Only allow my current IP | `POST /api/panel/allowlist/{preview,apply}` (lockout). **P3** (Q7). | P3 |
| Turn on firewall now | `POST /api/firewall/{preview,apply}` | P1 |
| Step 3 scan results (sites, DBs, supervisor, cron, certs, default site) with toggles | `GET /api/import/scan` | P1 |
| "Import N items" / undo | `POST /api/import/{preview,apply}`; `DELETE /api/import/{id}` | P1 |
| Step 4 Connect GitHub: Install GitHub App, site → repo matches | `GET /api/import/github-matches`; per-site `POST /api/projects/{id}/github/verify`. **GitHub App: Q2.** | P2 |
| "How it fits together" diagram | static | — |

---

## 4. Open questions (cross-cutting)
- **Q1. State-class writes are not Changes.** Advisor dismiss and snooze, plan and
  destination records, and deploy settings are audited but not snapshotted or rolled
  back. The brief said "all mutating endpoints". These writes are classed S because
  they touch no system file. Confirm, or have Phase 0 snapshot SQLite rows too.
- **Q2. GitHub App vs deploy key + PAT.** The mockups show a GitHub App (install, repo
  picker, "7 repos", auto-stored secrets). The roadmap says per-site deploy key +
  webhook secret + optional PAT. These contracts follow the roadmap. The repo picker,
  account header and Actions secret upload work only when a PAT is stored. Should a
  GitHub App come later?
- **Q3. MariaDB beside MySQL.** Ubuntu's `mariadb-server` and `mysql-server` packages
  conflict, so MariaDB on port 3307 would need a container or tarball. The proposal is
  to install MariaDB only when MySQL is absent, and to fix the mockup copy.
- **Q4. Changing a site's primary domain.** The id is immutable. Is "create new site +
  redirect" acceptable, or do we need a rename that migrates certs, pools and units?
- **Q5. Install wizard step 1.** The mockup has a one-time setup code at `/setup`,
  `nexacloud self-update`, and `uninstall --keep-sites`. None of these exist, and they
  are not in the roadmap. Keep v0.1 behaviour (password printed once; re-run
  install.sh to upgrade) unless this is approved.
- **Q6. Copy that overclaims.**
  - ".env encrypted at rest": the live `.env` must be plaintext for PHP. Only the
    snapshots and backups of it are protected: snapshots are root-only 0600, and
    backups are crypted.
  - "AES-256": rclone crypt uses XSalsa20-Poly1305.
  - "Hostinger firewall 0 rules": we can only see UFW.

  The mockup text should change.
- **Q7. Panel IP allowlist.** This is lockout-tier and was not in the roadmap. It is
  deferred to P3 with the firewall tools.
- **Q8. Deploy failure notifications** (Email + Slack). Not in the roadmap, so OUT for
  P2. Could be a generic webhook URL later.
- **Q9. MongoDB and Meilisearch** in the catalog. Not in the roadmap, so OUT.
- **Q10. [P0-ext] items need the Phase 0 owner's agreement.** These are:
  - job `steps` and `result`, and one-time secrets in `result`
  - the SSE `step` event and `id:`
  - verify `expect:"not_5xx"|"same_class"` and `resolve_to`
  - staged commands, or chained changes
  - D-class changes with no files and no job
  - `POST /api/changes/{id}/confirm` with the revert timer
  - the `code` field on errors
  - `secret` / `tier` / `steps` / `reversible` / `warnings` on previews
  - commands that need stdin, using `stdin_ref`
  - `snapshot_paths` for files changed by commands
  - ordered stages (commands before files)
  - redacted-hash rule R1 (§1.6), so plans with generated secrets keep a stable `plan_hash`
- **Q22. Sudo on every apply** (§1.9). Keep the Phase 0 precedent, or relax it for
  safe-tier changes?
- **Q23. Advisor rule ids.** Phase 0's first advisor fix is `nginx-hardening` (kebab
  case, a fixed path). These contracts use kebab-case rule ids in URLs
  (`/api/advisor/{rule}/…`). Keep `nginx-hardening` as the id of the `server_tokens`
  rule.
