# NexaCloud

A small, self-hosted control panel for **one VPS that runs Laravel and plain PHP sites**.
One Rust binary (the agent) runs on your server and serves a Nuxt web panel over HTTPS.

> **Status: v0.2 (early).** NexaCloud *reads* your server, and every change it makes goes
> through a safety pipeline: preview the exact diff, snapshot, apply, verify, revert
> automatically on failure, roll back later with one click (or `nexacloud rollback --last`).
> The first one-click fix ships now (hide the nginx version); creating sites, SSL, PHP
> installs and Git deploys build on this — see [docs/ROADMAP.md](docs/ROADMAP.md).

## What it does

- **Dashboard** — CPU, memory, disk, uptime, plus a "needs attention" list
  (end-of-life PHP, firewall off, stopped services, certificates about to expire, full disks).
- **Sites** — reads your nginx config and shows every site, whether it is Laravel or plain PHP,
  its PHP version, SSL expiry date and Git branch/commit.
- **PHP** — installed versions, which sites use each one, and their support status.
- **Services** — nginx, PHP-FPM, MySQL, PostgreSQL, Redis, Supervisor, cron… with a Restart button.
- **Databases & backups** — MySQL and PostgreSQL databases with sizes, one-click
  compressed backup, and download.
- **Security** — firewall (UFW), fail2ban, automatic updates, SSH password login, and the
  first one-click fix: hide the nginx version (Preview → Apply → Rollback).
- **Activity** — every change (with its diff, commands, checks and a Rollback button) and
  every background job (live log, cancel), kept across restarts.

Safety rules: NexaCloud only writes its own drop-in files (e.g. `/etc/nginx/conf.d/zz-nexacloud.conf`),
never your vendor or site files; dangerous actions ask for your password again ("sudo mode",
15 minutes); everything that changes the server is in an append-only audit log.

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
