# NexaCloud

A small, self-hosted control panel for **one VPS that runs Laravel and plain PHP sites**.
One Rust binary (the agent) runs on your server and serves a Nuxt web panel over HTTPS.

> **Status: v0.3.** Every change NexaCloud makes goes through a safety pipeline: preview
> the exact diff, snapshot, apply, verify (e.g. `nginx -t` + "does the site still answer?"),
> revert automatically on failure, roll back later with one click (or `nexacloud rollback --last`).

## What it does

- **Dashboard** — CPU, memory, disk, uptime, attention list, advisor score, backup status, recent changes.
- **Sites** — every nginx site (Laravel / plain PHP / static / proxy detected). **Create** sites,
  subdomains, redirects and proxies; **Let's Encrypt HTTPS**; per-site **PHP version switch**
  (managed and existing configs); security headers, upload limit, HSTS; a raw **nginx editor**
  that only saves configs passing `nginx -t`.
- **FrankenPHP** — switch any managed PHP site between PHP-FPM and FrankenPHP; NexaCloud runs it as
  its own systemd service behind nginx and switches back in one click.
- **Deployments** — connect a site to GitHub (read-only deploy key), deploy on push (webhook with
  HMAC or 1-minute polling), Laravel pipeline (composer, npm build, DB backup, migrate, optimize,
  queue:restart) run as the site user, **automatic rollback** when the site returns 5xx, generated
  GitHub Actions workflow, deploy history with roll-back-to-commit. Artisan runner, queue workers
  (Supervisor), scheduler (cron), `.env` editor, log viewer.
- **Databases** — phpMyAdmin-style browser for MySQL and PostgreSQL: tables, structure, rows
  (search, sort, paging), edit/insert/delete by primary key, SQL console (read-only by default,
  write mode needs your password, typed confirmation for DROP/TRUNCATE/WHERE-less DELETE),
  create database + user, reset passwords.
- **Backups** — scheduled plans (databases + folders), off-site copies to **Synology** (SFTP —
  Tailscale-friendly — with a panel SSH key), S3-compatible storage (Synology C2, AWS, MinIO, B2) or
  a local disk, 7 daily / 4 weekly / 3 monthly retention, restore into a copy (never over live data),
  disk usage + cleanup.
- **PHP & Services** — install PHP versions (ondrej PPA), extensions, php.ini settings, CLI default;
  one-click install of Redis, Supervisor, Fail2ban, UFW, Certbot, Composer, Node.js, rclone, ClamAV…
- **Advisor** — server score and tuned recommendations for your RAM (PHP OPcache, MySQL buffer pool,
  swap, gzip, DB bound to localhost, fail2ban, automatic updates, APP_DEBUG in production, backups…)
  with one-click fixes, dismiss and snooze.
- **Security** — firewall on with safe defaults and an **anti-lockout timer** (turns itself off unless
  you confirm within 3 minutes), fail2ban, updates, SSH checks.
- **Activity** — every change and job with diffs, live logs and Rollback; append-only audit log.

Safety rules: NexaCloud writes its own drop-in files where possible, dangerous actions ask for your
password again ("sudo mode", 15 minutes), code from your repos never runs as root, and everything
that changes the server is in the audit log.

## Install

On the server (Ubuntu 22.04 / 24.04, x86_64), as root:

```bash
curl -fsSL https://raw.githubusercontent.com/mhjarjis/nexacloud/main/scripts/install.sh | sudo bash
```

The installer:

1. downloads the latest release from GitHub and verifies its SHA-256 checksum,
2. installs to `/opt/nexacloud`, config in `/etc/nexacloud`, state (change history, job logs,
   rollback snapshots) in `/var/lib/nexacloud`, a `nexacloud` systemd service,
3. creates a random admin password and a self-signed HTTPS certificate,
4. prints the address (`https://YOUR_SERVER_IP:8443`) and the password **once**.

Your browser will warn about the certificate the first time — that's expected until the
panel gets its own domain (v0.2).

| Task | Command |
|---|---|
| Upgrade | re-run the install command |
| Forgot password | `sudo nexacloud reset-password` |
| Logs | `journalctl -u nexacloud -f` |
| Health check | `sudo nexacloud doctor` |
| Undo the last change | `sudo nexacloud rollback --last` (works with the service stopped) |
| Uninstall | `curl -fsSL https://raw.githubusercontent.com/mhjarjis/nexacloud/main/scripts/install.sh \| sudo bash -s -- --uninstall` |

If UFW is active, open the panel port: `sudo ufw allow 8443/tcp`.

## Develop locally

```bash
# 1. agent (plain HTTP on 127.0.0.1:8443, password "dev")
cd agent && cargo run -- serve --config dev-config.toml

# 2. web (proxies /api to the agent)
cd web && npm install && npm run dev
```

Build a release package the same way CI does:

```bash
(cd web && npm ci && npx nuxi generate)
(cd agent && cargo build --release)
bash scripts/package.sh   # → dist/nexacloud-x86_64-linux.tar.gz
```

### End-to-end test in Docker

Boots a throwaway Ubuntu 24.04 "VPS" (systemd, nginx, PHP 8.3-FPM, MySQL, PostgreSQL,
a sample Laravel site and a plain PHP site), runs the real installer and checks every
endpoint, backups, restarts, throttling, upgrade and uninstall:

```bash
bash tests/docker/run-e2e.sh
# keep it running and click around at https://127.0.0.1:18445:
KEEP=1 bash tests/docker/run-e2e.sh
node tests/docker/ui-check.mjs <password-it-prints>   # headless Chrome UI test + screenshots
docker rm -f nexacloud-e2e
```

Publishing a release: push a tag like `v0.2.0`; the Release workflow builds, runs the
install test on a fresh Ubuntu 24.04 runner, and attaches the package to a GitHub release.

## Layout

```
agent/     Rust agent (axum) — API + serves the web UI
web/       Nuxt panel (SPA)
scripts/   install.sh, package.sh
docs/      API.md — the contract between agent and web
```

## Security notes

- The agent runs as root because it reads nginx/PHP config and talks to systemd and the databases.
- Every command runs with fixed argument lists (no shell strings built from input) and validated names.
- HTTPS from the first start, HttpOnly + SameSite=Strict session cookie, login throttling,
  Origin check on every change request.
- Dangerous actions (apply/rollback a change, restart a service, cancel a job) need the
  password re-entered within the last 15 minutes.
- Every change is snapshotted in `/var/lib/nexacloud/snapshots/` before it is applied and is
  reverted automatically if `nginx -t` (or its other checks) fail.

## Roadmap (next)

- Create sites and subdomains (Laravel / plain PHP / static / proxy), with Let's Encrypt SSL
- Switch PHP version per site, install PHP versions
- Git deploys from GitHub (webhook, zero-downtime releases, rollback)
- Scheduled backups, restore into a copy, off-site copy (S3)
- One-click firewall with safe defaults (SSH + HTTP + HTTPS + panel port)
- Panel on its own domain with a real certificate
