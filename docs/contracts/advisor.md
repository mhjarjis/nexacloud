# Advisor, dashboard and metrics (Phase 1)

Conventions are in [README.md](README.md). The advisor **replaces and extends** the v0.1
alert rules. `GET /api/overview` keeps working: its `alerts[]` becomes a projection of
open findings.

**Sudo baseline.** Every class C or D `apply` needs sudo (README §1.9, following the
Phase 0 precedent). On preview/apply rows, the `Sudo` column marks whether the
**preview** also needs sudo. On J and S rows, it marks the endpoint itself.

## 1. Model

### 1.1 Finding (rule instance)
```json
{
  "key": "mysql-buffer-pool:mysql",
  "rule": "mysql-buffer-pool",
  "target": "mysql",
  "category": "security|performance|reliability|maintenance",
  "severity": "critical|warning|info",
  "title": "MySQL uses only 128 MB for its buffer pool",
  "detail": "Most of your 2.1 GB of data is read from disk. 1536M fits this 8 GB server.",
  "evidence": [
    {"label": "innodb_buffer_pool_size", "value": "134217728", "source": "SHOW GLOBAL VARIABLES"},
    {"label": "Buffer pool hit rate", "value": "93.1%", "source": "SHOW GLOBAL STATUS"}
  ],
  "current": "128M",
  "recommended": "1536M",
  "tier": "safe|restart|lockout|none",
  "restarts": [],
  "fix": {"kind": "change", "phase": "P1"},
  "state": "open|dismissed|snoozed",
  "snoozed_until": null, "dismiss_reason": null,
  "first_seen": "…", "last_seen": "…"
}
```
- `fix.kind`:
  - `change`: one-click preview and apply through the advisor endpoints.
  - `action`: a link to another endpoint, e.g.
    `{"kind":"action","label":"Back up now","method":"POST","path":"/api/backup/run-all"}`.
  - `manual`: `steps[]` text.
  - `none`.
- `fix.phase` is the phase in which the fix ships. Detection always ships in P1.
- Findings are keyed `rule:target`. A rule can have many instances, e.g. one per site.

### 1.2 Server budget
Recommended values come from the server's resources, not constants. The formulas below
reproduce the roadmap numbers for **8 GB / 2 vCPU**.

| Quantity | Formula | 8 GB / 2 vCPU |
|---|---|---|
| `ram_mb` | `MemTotal` | 7,936 (≈ 8 GB) |
| `reserve_os_mb` | 768 | 768 |
| `mysql_pool_mb` | round_down(ram_mb × 0.1875, 128) if MySQL/MariaDB installed, else 0 | **1536** |
| `mysql_overhead_mb` | 512 if MySQL is installed | 512 |
| `redis_mb` | 512 if Redis is installed, else 0 | 512 |
| `headroom_mb` | 1024 | 1024 |
| `fpm_budget_mb` | ram − all of the above | 3,584 |
| `avg_worker_mb` | median RSS of running `php-fpm: pool` processes (`/proc/<pid>/status` VmRSS); default 64 when there are none | 64 |
| `max_children` per site | clamp(floor(fpm_budget_mb / avg_worker_mb / n_php_sites), 4, 16) | 9 (6 sites) |
| `opcache.memory_consumption` | 256 if ram ≥ 4 GB, else 128 (per PHP version that has sites) | **256** |
| swap | 2 GB if ram ≤ 16 GB | **2G** |

`GET /api/advisor/budget` returns this table, so the UI can show "why".

## 2. Endpoints

| Method | Path | Class | Sudo | Locks | Phase |
|---|---|---|---|---|---|
| GET | `/api/advisor` | – | – | – | P1 |
| GET | `/api/advisor/budget` | – | – | – | P1 |
| POST | `/api/advisor/scan` | J (light) | – | – | P1 |
| POST | `/api/advisor/{rule}/preview` | C | per plan (lockout → ✓) | – | P1 |
| POST | `/api/advisor/{rule}/apply` | C | same | from the underlying plan (§3) | P1 |
| POST | `/api/advisor/apply-safe/preview` / `apply` | C (composite) | – | union | P1 |
| POST | `/api/advisor/{rule}/dismiss` | S | – | – | P1 |
| POST | `/api/advisor/{rule}/snooze` | S | – | – | P1 |
| POST | `/api/advisor/{rule}/restore` | S | – | – | P1 |

- `GET /api/advisor` returns:
  ```json
  {"scanned_at","score","security_score","counts":{"critical":2,"warning":3,"info":1},"findings":[…open…],"hidden":[…dismissed/snoozed…]}
  ```
  The ordering is severity, then category `security` first, then `first_seen`.
- Scans run every 15 min and after every applied change. A scan is cheap: no network,
  and the slowest probe is about 1 s.
- `{rule}/preview` body: `{"target":"mysql","params":{}}`. `params` can override the
  recommended value within that rule's validation, e.g. `{"value":"1024M"}`. The plan is
  produced **by the owning area's planner**, e.g. sites options or PHP ini. Advisor
  changes are therefore identical to manual ones and appear the same way in Activity.
- `apply-safe` body: `{"category":"security"|null,"keys":["…"]|null}`.
  - It takes every open finding with `tier:"safe"` and `fix.kind:"change"`.
  - It merges the plans into one Preview. Each file appears once. If two rules edit
    the same file, it is rendered once with both settings.
  - Restart and lockout findings are listed in `data.skipped[]` with a reason.
  - This is the Dashboard's "Apply recommended hardening" button.
- Dismiss body: `{"target":"…","reason":"…"}` (reason ≤ 200 chars).
- Snooze body: `{"target":"…","days":1-90}` or `{"until":"<ts>"}`.
- Restore clears both. It is keyed by `rule:target` in table
  `advisor_state(key, state, until, reason, at)`.
- An expired snooze reopens automatically. If evidence changes severity upward, e.g.
  warning → critical, a dismissed finding also reopens.

## 3. Rules

Every rule lists its detection, the recommendation for 8 GB / 2 vCPU, its tier, its
locks, and the exact content it writes. File content is the **full rendered managed
file** (README §1.13). Settings from other rules targeting the same file appear in the
same render.

### 3.1 Performance

**`fpm-ondemand`** (warning, restart: `php<v>-fpm` reload)
- **Detection.** Parse every `/etc/php/*/fpm/pool.d/*.conf` used by a site:
  - `pm` is not `ondemand`, or
  - Σ(max_children × avg_worker_mb) > `fpm_budget_mb`.
- **Fix:**
  - Managed pools: re-render with `pm = ondemand`,
    `pm.max_children = <budget value>`, `pm.process_idle_timeout = 10s`,
    `pm.max_requests = 500` (php-services.md §3.1).
  - Adopted `www` pool users: `fix.kind=change` maps to the `fpm-shared-pool` fix,
    because vendor files are never edited.
- Locks: `php:<v>`.

**`fpm-shared-pool`** (info, restart)
- **Detection.** A site's socket is `/run/php/php<v>-fpm.sock`, the `www` pool.
- **Fix:** adopt `own_pool` (sites.md §3.3).

**`opcache`** (warning, restart: `php<v>-fpm`)
- **Detection.** For each version with sites, check `php-fpm<v> -i`:
  - `opcache.enable` is not 1, or
  - `opcache.memory_consumption` < 256, or
  - `opcache.max_accelerated_files` < 10000.
- **Fix.** `/etc/php/<v>/fpm/conf.d/99-nexacloud.ini`:
  ```ini
  ; Managed by NexaCloud — do not edit; change values in the panel
  opcache.enable = 1
  opcache.memory_consumption = 256
  opcache.interned_strings_buffer = 16
  opcache.max_accelerated_files = 20000
  opcache.validate_timestamps = 1
  opcache.revalidate_freq = 2
  realpath_cache_size = 4096K
  realpath_cache_ttl = 600
  expose_php = Off
  memory_limit = 256M
  upload_max_filesize = 64M
  post_max_size = 64M
  ```
  `validate_timestamps` stays 1 because in-place deploys need it. Releases mode reloads
  FPM after the switch anyway.
- Locks: `php:<v>`.

**`mysql-buffer-pool`** (warning, **safe**)
- **Detection:**
  - `SELECT @@innodb_buffer_pool_size` < 1536M × 0.9, or
  - it is > 50 % of RAM.
- **Evidence:** the hit rate,
  `1 - Innodb_buffer_pool_reads / Innodb_buffer_pool_read_requests`.
- **Fix.** `/etc/mysql/mysql.conf.d/zz-nexacloud.cnf` (MariaDB: `mariadb.conf.d`). The
  full file, shown with `db_bind` and slow-log baseline settings also applied:
  ```ini
  # Managed by NexaCloud
  [mysqld]
  innodb_buffer_pool_size = 1536M
  bind-address = 127.0.0.1
  mysqlx-bind-address = 127.0.0.1
  slow_query_log = ON
  slow_query_log_file = /var/log/mysql/mysql-slow.log
  long_query_time = 1
  ```
  `mysqlx-bind-address` is written **only** for MySQL. MariaDB refuses unknown options.
- **Apply.** Write the file, then resize online with
  `SET GLOBAL innodb_buffer_pool_size = 1610612736`. This is dynamic since 5.7 /
  MariaDB 10.2, so no restart is needed.
- **Verify.** Poll `SHOW STATUS LIKE 'Innodb_buffer_pool_resize_status'` until it
  reports completed (≤ 120 s).
- Locks: `db-server:mysql`.

**`swap-missing`** (warning, safe)
- **Detection:**
  - `SwapTotal` = 0 in `/proc/meminfo`, and
  - the root filesystem is not btrfs (`findmnt -no FSTYPE /`), and
  - free space is ≥ 6 GB.
- **Fix:**
  - `fallocate -l 2G /swapfile`. If that fails:
    `dd if=/dev/zero of=/swapfile bs=1M count=2048 status=none`.
  - `chmod 600 /swapfile`
  - `mkswap /swapfile`
  - `systemctl daemon-reload`
  - `systemctl enable --now swapfile.swap`
  - `sysctl -p /etc/sysctl.d/99-nexacloud.conf`
- **Files.** It uses a systemd unit, not `/etc/fstab`, so no vendor file is touched.

  `/etc/systemd/system/swapfile.swap` (the unit name must equal the escaped path):
  ```ini
  [Unit]
  Description=NexaCloud swap file (2G)
  [Swap]
  What=/swapfile
  [Install]
  WantedBy=swap.target
  ```
  `/etc/sysctl.d/99-nexacloud.conf`:
  ```
  # Managed by NexaCloud
  vm.swappiness = 10
  ```
- **Rollback.** `swapoff /swapfile`, remove the unit, `rm /swapfile`.
- Locks: `swap`, `sysctl`.

**`nginx-http2`**, **`nginx-gzip`** (info, safe)
- **Detection.** Per site with SSL: no `http2` on any 443 listen (and no `http2 on;`),
  or no gzip (`gzip on` in the effective http or server context).
- **Fix:** managed sites only:
  `POST /api/sites/{id}/options/preview` / `apply` with `{"http2":true}` or `{"gzip":true}`. For unmanaged
  sites, `fix.kind="manual"` says "Adopt the site to enable".
- Locks: `site:<id>`, `nginx`.

**`upload-limits`** (warning, restart: `php<v>-fpm` reload)
- **Detection.** For each PHP site, find the effective values of:
  - nginx `client_max_body_size` (default 1M)
  - PHP `upload_max_filesize` (default 2M)
  - PHP `post_max_size` (default 8M)

  The rule fires when any of these is true:
  - they are not all equal to the recommended value
  - `post_max_size` < `upload_max_filesize`
  - nginx's limit < `upload_max_filesize`
- **Recommendation:** 64M everywhere.
- **Fix:** site options `client_max_body_size: 64M` plus pool `php_admin_value`s in one
  plan (sites.md §8).

**`queue-not-redis`** (info, restart: the site's supervisor workers)
- **Detection.** A Laravel site's `.env` has `QUEUE_CONNECTION` ∈ {`sync`, `database`}
  **and** the site has workers or scheduler entries.
- **Preconditions,** shown in the preview:
  - Redis is installed (`422 precondition_failed`, `missing:["redis"]`).
  - `php<v>-redis` is installed, or `predis/predis` is in `composer.lock`.
  - For `database`: `SELECT COUNT(*) FROM jobs` is 0. Otherwise the preview warns that
    N jobs would be stranded and that the queue should be drained first.
- **Fix,** one change:
  1. `.env`: `QUEUE_CONNECTION=redis`, plus `REDIS_HOST=127.0.0.1` if unset.
  2. `php artisan config:cache`, only if `bootstrap/cache/config.php` exists.
  3. Supervisor programs: `queue:work redis …`, then `supervisorctl update`.
  4. `php artisan queue:restart`.
- Locks: `site:<id>`, `supervisor`. Phase: detection P1, fix **P2**, because it needs
  the workers and `.env` contracts.

### 3.2 Security

**`ufw-inactive`** (critical, **lockout**)
- **Detection.** `ufw` is not installed, or `ufw status` is inactive, or there are 0
  allow rules.
- **Fix:** `POST /api/firewall/preview {"preset":"defaults"}` (php-services.md §8), with
  confirm-or-revert. If not installed, the plan includes `apt-get install ufw`.
- Locks: `firewall` (+ `apt`).

**`db-public-bind`** (critical, restart: `mysql` / `postgresql` / `redis-server`)
- **Detection.** `ss -Hltn` shows a listener on 3306, 33060, 5432 or 6379 bound to an
  address that is not `127.0.0.1`, `::1` or `localhost`. Evidence: the `ss` line.
- **Fix:**
  - MySQL: `zz-nexacloud.cnf` with `bind-address = 127.0.0.1` (+ mysqlx).
  - PostgreSQL: `/etc/postgresql/<v>/main/conf.d/zz-nexacloud.conf`:
    ```
    # Managed by NexaCloud
    listen_addresses = 'localhost'
    ```
  - Redis: `bind 127.0.0.1 -::1` in `/etc/nexacloud/redis.conf` (php-services.md §7.1).
- **Restart.** The named unit restarts. Verify with `ss` after the restart.
- **Warning.** If a DB user has a `host` other than `localhost` / `127.0.0.1` / `::1`,
  the preview warns that remote clients will be cut off.
- Locks: `db-server:<engine>` / `redis`.

**`cert-expiry`** (warning < 14 d, critical when expired, safe)
- **Detection.** `ssl.days_left` per site, plus `systemctl is-active certbot.timer`.
- **Fix:** `POST /api/sites/{id}/ssl/apply {"action":"renew"}`.
- **Related rule `certbot-timer`** (warning): fix `systemctl enable --now certbot.timer`.

**`fail2ban-missing`** (warning, safe)
- **Detection.** The package is missing, or the `sshd` jail is not active
  (`fail2ban-client status sshd`).
- **Fix:** catalog install `fail2ban` (the jail file is in php-services.md §7.1).
- Locks: `apt`, `fail2ban`.

**`unattended-upgrades-off`** (warning, safe)
- **Detection:**
  - `dpkg -s unattended-upgrades` is missing, or
  - `apt-config dump APT::Periodic::Unattended-Upgrade` is not `"1"`, or
  - `systemctl is-enabled apt-daily-upgrade.timer` is not `enabled`.
- **Fix:**
  1. Install the package if missing.
  2. Write `/etc/apt/apt.conf.d/21nexacloud-auto-upgrades`. It sorts after the vendor
     `20auto-upgrades`, so it wins:
     ```
     // Managed by NexaCloud
     APT::Periodic::Update-Package-Lists "1";
     APT::Periodic::Unattended-Upgrade "1";
     APT::Periodic::AutocleanInterval "7";
     ```
  3. Write `/etc/apt/apt.conf.d/52nexacloud-unattended-upgrades`:
     ```
     // Managed by NexaCloud
     Unattended-Upgrade::Automatic-Reboot "false";
     Unattended-Upgrade::Remove-Unused-Kernel-Packages "true";
     ```
  4. Run `systemctl enable --now apt-daily.timer apt-daily-upgrade.timer`.
- Locks: `apt`.

**`app-debug-production`** (critical, safe)
- **Detection.** A Laravel site's `.env` has `APP_ENV=production` and
  `APP_DEBUG=true` (case-insensitive; also `1`).
- **Fix:**
  1. `.env` change `APP_DEBUG=false` (projects-deploys.md §9.7 write path).
  2. If `bootstrap/cache/config.php` exists:
     `systemd-run … php<v> artisan config:cache` as the site user.
- In P1, the `.env` write uses the same planner, which ships in P1 for this rule alone.
- Locks: `site:<id>`.

**`malware-scanner-missing`** (warning, safe)
- **Fix:** catalog install `clamav` (php-services.md §7.1).

**`malware-found`** (critical, none)
- **Detection.** `FOUND` lines in the last `/var/log/nexacloud/clamscan.log`.
- **Fix:** `manual`. It lists the files. NexaCloud never deletes them.

**`ssh-password-auth`** (warning, lockout)
- **Detection.** `sshd -T` shows `passwordauthentication yes`.
- **Fix:** **P3** (php-services.md §8). In P1 the fix is `manual`.

**`xdebug-in-fpm`** (warning, restart)
- **Detection.** `php-fpm<v> -m` lists `xdebug`.
- **Fix:** `phpdismod -v <v> -s fpm xdebug` (php-services.md §4).

**`nginx-hardening`** (info, safe). **This rule already exists in Phase 0**
(`POST /api/advisor/nginx-hardening/{preview,apply}`, docs/API.md). Phase 1 only lists
it as a finding.
- **Detection.** `server_tokens off` is absent from the effective http context
  (`nginx -T`).
- **Fix:** Phase 0's change, which writes `/etc/nginx/conf.d/zz-nexacloud.conf`. Phase 1
  keeps that file as the shared http-context file (sites.md §7.2).

**`php-eol`** (critical) and **`php-ends-soon`** (warning, < 90 days)
- **Detection.** The v0.1 alert logic, per site.
- **Fix:** `action` → the PHP page, per-site switch (php-services.md §3.3).

### 3.3 Reliability and maintenance

**`backup-offsite-stale`** (warning, or critical when > 72 h)
- **Detection.** For every database target, take the newest
  `backup_copies.verified_at` on a non-local destination.
  - No off-site destination at all: warning, "No off-site copy".
  - Age > 26 h for a daily plan, or > 8 d for a weekly plan: warning.
  - Age > 72 h: critical.
- **Fix:** `action` `POST /api/backup/run-all`. Without a destination, `action` links to
  `/backups#destinations`.

**`logrotate-missing`** (warning, safe)
- **Detection.** For any site, `storage/logs/laravel.log` or the worker logs are larger
  than 100 MB **and** no `/etc/logrotate.d/*` file covers that path. Coverage is checked
  with `logrotate --debug` dry parse, or by matching globs.
- **Fix.** `/etc/logrotate.d/nexacloud-<slug>`:
  ```
  # Managed by NexaCloud
  /var/www/shop.example.com/shared/storage/logs/laravel.log
  /var/log/nexacloud/workers/nc-shop-*.log
  /var/log/nexacloud/scheduler/shop.log {
      su nc_shop nc_shop
      daily
      rotate 14
      maxsize 100M
      missingok
      notifempty
      compress
      delaycompress
      copytruncate
      dateext
  }
  ```
  For in-place sites the first path is `<base>/storage/logs/laravel.log`.
- **Verify.** `logrotate --debug /etc/logrotate.d/nexacloud-<slug>` exits 0.
- Locks: `logrotate`.

**`disk-full`** (warning > 85 %, critical > 95 %)
- **Fix:** `action` → disk cleanup (backups.md §7).

**`service-down`** (critical)
- **Detection.** The v0.1 rule: a known service is installed but not active.
- **Fix:** `action` `POST /api/services/{unit}/restart`.

**`supervisor-stalled`** (warning)
- **Detection.** See projects-deploys.md §9.4.
- **Fix:** `action` restart worker. **P2.**

**`redis-maxmemory`** (info, restart: redis)
- **Detection.** `CONFIG GET maxmemory` = 0.
- **Fix:** `maxmemory 512mb` and `maxmemory-policy volatile-lru` (php-services.md §7.1).

**`default-site-enabled`** (info, safe)
- **Detection.** `/etc/nginx/sites-enabled/default` exists, and no imported site depends
  on it.
- **Fix:** remove the symlink only, with the nginx write path.

## 4. Score
- **Formula.** `score = max(0, 100 − Σ weight)` over **open** findings, with
  `weight = {critical: 20, warning: 8, info: 2}`.
- **Caps.** Each rule counts at most `2 × weight`, however many targets it has.
- **Security score.** The same formula, restricted to `category = "security"`. The
  Dashboard shows this as "Score N / 100".
- **Hidden findings.** Dismissed and snoozed findings do not count. The UI shows "N
  hidden".

## 5. Dashboard and metrics

### 5.1 Metrics sampler (P1)
A tokio task samples every 60 s and stores into SQLite:
- `cpu` (%), `mem_used`, `swap_used`, `load1`
- `disk_used` per mount
- `rx`, `tx`: byte deltas summed over non-`lo` interfaces from `/proc/net/dev`. A
  negative delta means the counter reset, and the sample is recorded as 0.

| Table | Resolution | Retention |
|---|---|---|
| `metrics_1m` | 1 min | 48 h |
| `metrics_5m` | 5 min averages; rx/tx summed | 30 d |
| `metrics_1d` | daily max `disk_used` and summed rx/tx | 400 d. This feeds disk growth (backups.md §7) and monthly bandwidth. |

`GET /api/metrics?range=1h|24h|7d|30d&series=cpu,mem,swap,load,rx,tx` returns
`{"step_s":60,"points":[{"t":"…","cpu":6.2,"mem":3495100416,"rx":1200,"tx":4100}]}`.
- `1h` and `24h` read `metrics_1m`. For `24h`, the server downsamples to 5 min.
- `7d` and `30d` read `metrics_5m`. For `30d`, the server downsamples to 1 h.

### 5.2 `GET /api/dashboard` (P1)
One call per Dashboard load. Each section degrades to `null` independently.
```json
{
  "server": {"hostname": "nexadev.io", "os": "Ubuntu 24.04", "virt": "kvm", "cpu_count": 2, "ram": 8323477504},
  "nav_badges": {"projects": 2},
  "attention": [{"level": "critical", "text": "portal.nexadev.io runs PHP 8.1, which no longer gets security fixes.", "action": {"label": "Upgrade", "link": "/php"}, "key": "php-eol:portal.nexadev.io"}],
  "system": {"cpu_percent": 6.0, "cpu_24h": [5.1, 6.0], "mem_used": 3495100416, "mem_total": 8323477504, "disk_used": 40802189312, "disk_total": 107374182400},
  "bandwidth": {"month_rx": 9663676416, "month_tx": 9663676416, "rx_24h": 4900000, "tx_24h": 37600000, "quota": null},
  "projects": [{"id": "shop.nexadev.io", "repo": "nexadev/shop", "branch": "main", "framework": "Laravel 12", "php": "8.3", "status": "healthy|warning|critical", "status_text": "Healthy", "p95_ms": 142, "uptime_30d": 99.98, "last_deploy_at": "…"}],
  "services": [{"name": "Nginx", "meta": "1.24.0 · 9 domains", "state": "running|stopped|degraded|not_installed", "state_text": "Running"}],
  "deploys": [],
  "backups": {"last_run": {"status": "ok", "at": "…", "databases": 6, "bytes": 2576980377}, "next_run_at": "…", "retention": "7 daily · 4 weekly · 3 monthly", "offsite": {"label": "S3 · eu-central-1", "last_verified_at": "…"}, "provider_snapshots": null},
  "security": {"score": 62, "items": [
    {"name": "Firewall", "state_text": "0 rules", "level": "bad", "rule": "ufw-inactive"},
    {"name": "SSL certificates", "state_text": "8 valid · 1 waiting", "level": "ok"},
    {"name": "Malware scanner", "state_text": "Not installed", "level": "warn", "rule": "malware-scanner-missing"},
    {"name": "Fail2ban (SSH)", "state_text": "Not installed", "level": "warn", "rule": "fail2ban-missing"},
    {"name": "OS security updates", "state_text": "Auto-apply on", "level": "ok", "rule": "unattended-upgrades-off"}]}
}
```
- `attention`: open findings with severity ≥ warning, sorted, top 6. `text` is the
  finding title. `action` comes from `fix`.
- `cpu_24h`: 96 points, which are 15-min averages.
- `virt`: from `systemd-detect-virt`.
- `bandwidth.quota`: optional setting `bandwidth_quota_bytes` (S, `PUT /api/settings`),
  e.g. the "8 TB" plan limit.
- `projects`: the health prober (P1, projects-deploys.md §9.1) supplies status, p95 and
  uptime. `repo` and `last_deploy_at` are null until P2.
- `services` meta strings are computed server-side:
  - Nginx: version · domain count
  - PHP-FPM: the versions that have pools
  - MySQL: version · DB count
  - Redis: version · `used_memory_human`
  - Supervisor: worker count, or "N stalled"
  - Cron / Scheduler: "schedule:run every minute" when NexaCloud cron files exist
- `deploys`: the last 4 runs (P2). Empty in P1.
- `provider_snapshots`: `null`, or `{"label":"Hostinger","url":"<provider_panel_url>"}`.
  This is a link only (README matrix: OUT).
