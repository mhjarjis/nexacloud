# NexaCloud roadmap (v0.2 → v0.4)

Goal: every screen in the approved design (Dashboard, Projects, Project detail,
Domains & SSL, PHP & Services, GitHub CI/CD, Databases, Backups & Storage,
Install & Import) working against the real server, plus a phpMyAdmin-style
database browser, off-site backups (Synology / S3 / SFTP), FrankenPHP as an
alternative runtime, and a server advisor with one-click fixes.

The panel becomes root-capable, so **no mutating feature ships before the
safety foundation (phase 0)**. Keep the dark graphite design; no visual overhaul.

## Phase 0 — Safety foundation (v0.2.0) — done
Implemented: `db` (SQLite + migrations), `jobs` (persistent queue, heavy
semaphore, named locks, timeouts, cancel, SSE logs), `changes` (plan/preview/
snapshot/apply/verify/auto-revert/rollback with drift check), `audit`, sudo
mode, CLI `rollback`/`doctor`, Activity page, `ChangePreview`/`SudoPrompt`
components, and the first change type (nginx "hide version" drop-in, in
`advisor::nginx_hardening` — the template for later fixes). Contract: docs/API.md v0.2.

- TODO: optional TOTP as a second factor for login and sudo mode (not built yet).

- SQLite state at `/var/lib/nexacloud/state.db` (rusqlite, bundled, WAL).
- Persistent job queue: one heavy job at a time, per-resource locks (`site:x`,
  `nginx`, `db:x`, `apt`), per-job timeouts, cancel, log files on disk streamed
  over SSE, jobs running at startup marked `interrupted`.
- **Change pipeline** for every mutation: plan (file diffs + commands) →
  preview (`dry_run`) → snapshot → apply → verify → auto-revert on failure.
  Snapshots in `/var/lib/nexacloud/snapshots/<change_id>/`; one-click rollback;
  CLI `nexacloud rollback --last`, `nexacloud doctor`.
- Append-only audit log (time, IP, action, target, diff hash, result).
- "Sudo mode": re-enter password for dangerous actions, valid 15 min (TOTP: TODO above).
- Drop-in config files only (`zz-nexacloud.cnf`, `99-nexacloud.ini`, …), never edit vendor files.
- Web: Activity timeline (replaces Jobs) with plan/diff view and Rollback per change.

## Phase 1 — Sites, SSL, PHP, backups, advisor (v0.2.x)
- **Sites & domains**: managed vs unmanaged sites; Adopt flow (previewed diff);
  create site / subdomain / redirect / proxy; DNS check; Let's Encrypt via
  certbot webroot; safe nginx editor (visual toggles + raw, one write path:
  snapshot → atomic replace → `nginx -t` → reload → probe → auto-restore);
  drift detection; panel on its own domain (`panel.example.com`), keeping :8443 as break-glass.
- **PHP**: install PHP versions (ondrej PPA), switch PHP per site (new pool →
  probe → swap → rollback), extensions, php.ini drop-ins, CLI default.
- **Services**: install/enable Redis, Supervisor, UFW, Fail2ban, ClamAV, MariaDB, …
- **Backups**: schedules (internal scheduler), rclone destinations — SFTP
  (Synology via Tailscale or DDNS), S3-compatible (Synology C2, MinIO, AWS),
  WebDAV; rclone crypt; retention 7d/4w/3m; restore into `<db>_restore_<date>`
  then explicit swap; integrity checks; disk breakdown + cleanup.
- **Advisor**: rules with evidence + recommended value + Change (safe tier one
  click, restart tier names the service, lockout tier confirm-or-revert);
  dismiss/snooze. Budget for 8 GB/2 vCPU: fpm ondemand, opcache 256M,
  innodb_buffer_pool 1536M, 2 GB swap, http2/gzip, upload limits, cert expiry,
  ufw, DB bound to localhost, fail2ban, unattended upgrades, logrotate,
  APP_DEBUG=false, Redis for queues, off-site backup freshness.
- **Dashboard** to design parity (project health, deploys, backups, security score).

## Phase 2 — Projects, deploys, database manager (v0.3)
- Projects (Laravel / plain PHP / static), New project wizard, Install & Import wizard.
- Git deploys: per-site ed25519 deploy key + webhook secret (optional PAT to
  auto-install both); in-place mode for adopted sites, releases mode
  (`releases/<ts>`, `shared/`, `current`, keep 5); pipeline: clone → composer →
  npm build → pre-migrate DB backup → migrate → switch → reload → queue:restart
  → probe → auto-rollback; runs as the site user via `systemd-run` with MemoryMax;
  webhook `/hooks/github/{site}` (HMAC, rate-limited) or `git ls-remote` polling;
  generated GitHub Actions workflow.
- Project detail: health checks, response time, Artisan runner, queue workers,
  scheduler, `.env` editor, live logs, maintenance mode.
- Database manager: dedicated `nexacloud_admin` user; browse tables/rows with
  pagination, edit by primary key, SQL console (read-only by default, write mode
  needs sudo, 1000-row cap, 30 s timeout, typed confirm for DROP/TRUNCATE/
  WHERE-less DELETE, query audit); create DB / user / grants; slow queries.

## Phase 3 — FrankenPHP & lockout-tier tools (v0.4)
- FrankenPHP per site: systemd unit on a loopback port behind nginx; classic
  mode default, worker mode only with Octane; preflight PHP/extension diff,
  probe, swap, auto-rollback; keep the fpm pool for one-click switch back.
- Firewall / SSH changes with confirm-within-60s or auto-revert timer.
- PostgreSQL parity in the database manager.

## Not planned (for now)
Multi-user/roles, multi-server, web terminal, file manager, DNS/mail hosting,
wildcard certificates, Docker app hosting.

## Testing
Every phase extends `tests/docker/run-e2e.sh` (Ubuntu 24.04 + systemd) with
stand-ins: Pebble (ACME), an SFTP container and MinIO (backups), a local bare
Git repo + signed fake webhook (deploys). CI runs the install test on 22.04 + 24.04.
