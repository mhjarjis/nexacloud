# PHP versions, extensions and services (Phase 1)

Conventions are in [README.md](README.md). All apt commands run with:
- env `DEBIAN_FRONTEND=noninteractive`, `LC_ALL=C.UTF-8`, `NEEDRESTART_MODE=a`
- argv prefix `apt-get -y -q -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold`

Every apt job takes the `apt` lock and is **heavy**.

**Sudo baseline.** Every class C or D `apply` needs sudo (README §1.9, following the
Phase 0 precedent). On preview/apply rows, the `Sudo` column marks whether the
**preview** also needs sudo. On J and S rows, it marks the endpoint itself.

## 1. PHP overview

`GET /api/php` (P1). This extends v0.1, and the v0.1 fields are unchanged.
```json
{
  "cli_default": "8.3",
  "ppa": {"configured": true, "name": "ppa:ondrej/php"},
  "available": ["7.4", "8.0", "8.1", "8.2", "8.3", "8.4", "8.5"],
  "recommended": "8.4",
  "versions": [
    {"version": "8.3", "fpm_installed": true, "fpm_active": true, "eol": false, "security_until": "2027-12-31",
     "support": "active|security|eol", "state": "installed|installing|ends_soon|eol",
     "sites": ["shop.example.com"], "pools": ["shop", "api"], "www_pool": true,
     "install_job_id": null, "extensions_count": 24}
  ]
}
```
- `available`:
  - When the PPA is configured, it lists the `php<v>-fpm` packages found with
    `apt-cache policy` that have a candidate.
  - Without the PPA, it lists the distro version: 8.1 on 22.04, 8.3 on 24.04.
- `recommended` is the newest version in `available` with `support=active` that is
  ≥ 30 days past its GA.
- `support` comes from the v0.1 table plus active-support end dates (static table in
  `php.rs`):

  | Version | Active support ends |
  |---|---|
  | 8.3 | 2025-12-31 |
  | 8.4 | 2026-12-31 |
  | 8.5 | 2027-12-31 |
- `state` is `ends_soon` when `security_until` is less than 90 days away.
- `install_job_id` is set while an install job is queued or running, so the UI can
  resume the progress card.

`GET /api/php/sites` (P1) feeds the "PHP version per site" table:
```json
[{"site_id": "portal.nexadev.io", "now": "8.1", "runtime": "fpm", "suggested": "8.3",
  "compat": {"status": "issues|compatible|not_scanned|scanning", "deprecations": 2, "errors": 0, "scanned_for": "8.3", "scanned_at": "…", "job_id": null}}]
```
`suggested` is the newest installed version that is newer than `now`. If no newer
version is installed, it is `recommended`.

## 2. Install and uninstall PHP versions

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/php/install/prepare` | J (cancellable) | – | `apt` (heavy) |
| POST | `/api/php/install/preview` / `apply` | C, `reversible:false` | – | `apt`, `php:<v>` (heavy) |
| POST | `/api/php/{ver}/uninstall/preview` / `apply` | C, `reversible:false` | ✓ | `apt`, `php:<v>` |

**Install body.** `{"version":"8.5","extensions":["bcmath","curl",…]}`.

When `extensions` is omitted, the default set is:
`cli fpm common bcmath curl gd intl mbstring mysql pgsql redis sqlite3 xml zip opcache readline`.

- Packages are `php<v>-<ext>` for each item.
- `opcache` is included **only if** `apt-cache show php<v>-opcache` succeeds. PHP 8.5
  bundles OPcache into core; verify at implementation.
- The progress card shows "N extensions": the count of non-SAPI packages.

**Steps.** These match the Runtimes mockup's five steps. Phase 0 change jobs cannot
be cancelled once running, so steps 1–2 are a separate **prepare** job (J), and steps
3–5 are the change. The UI chains them: prepare, then preview, then apply. It shows one
progress card with five steps.

1. **Add apt repository.** Skipped if `/etc/apt/sources.list.d/ondrej-ubuntu-php-*.{list,sources}`
   exists. Commands:
   - `apt-get install software-properties-common` (if missing)
   - `add-apt-repository -y --no-update ppa:ondrej/php`
   - `apt-get update`
2. **Download packages.** `apt-get install --download-only php<v>-fpm …`. This is the
   end of the cancellable prepare job.
3. **Install php<v>-fpm + extensions.** `apt-get install php<v>-fpm php<v>-cli …`. This
   is the start of the change, which is not cancellable once running.
4. **Create FPM pool / configure.** The ini and probe-pool files are part of the
   change's `files`. Phase 0 writes files **before** commands, and `/etc/php/<v>/…`
   does not exist until dpkg has run. This change therefore needs the [P0-ext]
   "commands before files" stage (README §1.6). Until that exists, it is split into two
   chained changes: (a) apt install, then (b) drop-ins + reload + health check.
   - Write `/etc/php/<v>/fpm/conf.d/99-nexacloud.ini` and
     `/etc/php/<v>/cli/conf.d/99-nexacloud.ini` from the current managed ini settings
     (§5).
   - Write the probe pool `/etc/php/<v>/fpm/pool.d/zz-nexacloud-probe.conf` (§3.2).
   - Run `php-fpm<v> -t`, then `systemctl enable --now php<v>-fpm`, then
     `systemctl reload php<v>-fpm`.
5. **Health check.**
   - `php<v> -v` succeeds.
   - `systemctl is-active php<v>-fpm` is `active`.
   - A FastCGI ping to the probe socket returns `pong` (§3.2).

The job `result` is `{"version":"8.5","packages":[…],"extensions":[…]}`.

"Cancel install" is Phase 0 `POST /api/jobs/{id}/cancel` (sudo) on the **prepare** job.
After a cancel, any downloaded `.deb` files are left in the apt cache. The ondrej PPA
added by prepare stays. It is reported in the job result as `repo_added:true`, and the
audit log records it.

**Uninstall.**
- Refused with `409 conflict` while any site or the CLI default uses the version.
- Body: `{"confirm":"php<v>"}`.
- Command: `apt-get purge php<v>-*`. The argv contains each package name explicitly,
  taken from `dpkg-query -W -f='${Package}\n' 'php<v>-*'`.
- Then: `apt-get autoremove --purge`.

## 3. Pools, probe and per-site switch

### 3.1 Per-site pool `/etc/php/<v>/fpm/pool.d/<slug>.conf`
Rendered whole. Values are for an 8 GB / 2 vCPU budget; see advisor.md.
```ini
; Managed by NexaCloud — site shop.example.com
[shop]
user = nc_shop
group = nc_shop
listen = /run/php/php8.3-fpm-shop.sock
listen.owner = www-data
listen.group = www-data
listen.mode = 0660

pm = ondemand
pm.max_children = 8
pm.process_idle_timeout = 10s
pm.max_requests = 500
request_terminate_timeout = 120s

ping.path = /nc-fpm-ping
ping.response = pong
pm.status_path = /nc-fpm-status

catch_workers_output = yes
decorate_workers_output = no
php_admin_value[error_log] = /var/log/php/8.3-shop.error.log
php_admin_flag[log_errors] = on
php_admin_value[upload_max_filesize] = 64M
php_admin_value[post_max_size] = 64M
; per-site overrides (from PUT site php settings) follow
```
- `/var/log/php/` has mode 0755. Each log file is `nc_<slug>`-owned, mode 0640.
- **Adopted sites** that use the shared `www` pool keep it. NexaCloud never edits
  `www.conf`. Moving to an own pool is the adopt option `own_pool` (sites.md §3.3) or
  the advisor rule `fpm-shared-pool`.
- **Per-site overrides**:
  - Allowlisted keys: `memory_limit`, `upload_max_filesize`, `post_max_size`,
    `max_execution_time`, `max_input_vars`.
  - Validation regexes are in §5.
  - Stored in the `sites` table.

### 3.2 Probe
`zz-nexacloud-probe.conf` is a tiny pool:
```ini
[nexacloud-probe]
user = www-data
group = www-data
listen = /run/php/php<v>-fpm-nexacloud-probe.sock
listen.owner = root
listen.group = root
listen.mode = 0600
pm = ondemand
pm.max_children = 1
ping.path = /nc-fpm-ping
ping.response = pong
```
- The agent has a minimal **FastCGI client** (Rust). It sends
  `SCRIPT_NAME=/nc-fpm-ping`, `SCRIPT_FILENAME=/nc-fpm-ping` and
  `REQUEST_METHOD=GET`, and expects a body of `pong`.
- Site pools are probed the same way, through their own socket. This proves both the
  master and the pool user work, without needing a PHP file on disk.

### 3.3 Per-site PHP switch
| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/sites/{id}/php/scan` | J | – | `site:{id}` |
| POST | `/api/sites/{id}/php/preview` | C | – | – |
| POST | `/api/sites/{id}/php/apply` | C (`tier: restart`, reloads php<new>-fpm, php<old>-fpm and nginx) | – | `site:{id}`, `nginx`, `php:<old>`, `php:<new>` |

**Scan.** Body `{"target":"8.3"}`. The target must be installed. This is a heavy-ish job
with a 300 s timeout, run as the site user via
`systemd-run --uid=<user> --wait --pipe --property=MemoryMax=512M`.
1. **Lint.** `php<target> -l <file>` for each `*.php` under `app_root`. Excludes
   `vendor/`, `node_modules/`, `storage/`, `.git/`. Max 5,000 files; batches of 50 per
   process. Collect `Deprecated:` and `Parse error` lines.
2. **Platform requirements.** If `composer.json` exists:
   `php<target> /usr/local/bin/composer check-platform-reqs --no-dev --format=json --working-dir=<app_root>`.
   Failures count as `errors`.
3. **Runtime deprecations.** Count distinct `Deprecated:` messages in the site's
   current error log over the last 7 days. These are reported separately as
   `runtime_deprecations`.

Result:
```json
{"target":"8.3","status":"issues","deprecations":2,"errors":0,
 "items":[{"kind":"deprecation|parse_error|platform","file":"src/Model/Page.php","line":42,"message":"…"}],
 "runtime_deprecations":[{"message":"Creation of dynamic property …","count":88}]}
```
This is a cheap scan. It does not replace PHPCompatibility, and the UI copy should say
"quick scan".

**Switch.** Body `{"to":"8.4"}`. Flow ("new pool → probe → swap → rollback"):
1. Write `/etc/php/<new>/fpm/pool.d/<slug>.conf`. It is a copy of the old pool with the
   version substituted. Then run `php-fpm<new> -t` and `systemctl reload php<new>-fpm`.
2. Probe the new socket (§3.2). On failure, abort: remove the new pool and reload.
3. Re-render the vhost with the new `{{socket}}` through the nginx write path
   (sites.md §9), including its HTTP probe. The expected status class is recorded
   before the switch.
4. Update the site's scheduler cron, its supervisor `command=` binaries
   (`/usr/bin/php<new>`) and the deploy settings `php_binary`. These are part of the
   same change, with `supervisorctl update` for affected programs.
5. Remove the old pool file and reload `php<old>-fpm`. This is snapshotted, so rollback
   restores it.

A failure in steps 3–5 triggers Phase 0 auto-revert of every file plus the reloads.

## 4. Extensions

`GET /api/php/{ver}/extensions` (P1):
```json
[{"name":"redis","package":"php8.3-redis","installed":true,"enabled_fpm":true,"enabled_cli":true,"builtin":false},
 {"name":"swoole","package":"php8.3-swoole","installed":false,"enabled_fpm":false,"enabled_cli":false,"builtin":false}]
```
- **Sources:**
  - `php<v> -m`, for CLI.
  - `php-fpm<v> -m`, for FPM.
  - `/etc/php/<v>/mods-available/*.ini`.
  - `dpkg-query -W 'php<v>-*'`.
- **Catalog list:** bcmath, curl, gd, intl, mbstring, mysql, pgsql, sqlite3, redis, zip,
  xml, opcache, imagick, swoole, xdebug, igbinary, msgpack, soap, ldap, gmp, readline,
  memcached, mongodb, apcu.
- **Name to package:** `<ext>` maps to `php<v>-<ext>`. Exception: `mysql` covers
  mysqli, mysqlnd and pdo_mysql.
- `builtin:true` means compiled-in with no package. For example `ctype`, `json`, and
  `opcache` from 8.5.

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/php/{ver}/extensions/preview` / `apply` | C (`restart` tier: reload php<v>-fpm) | – | `apt` (only if installing), `php:<v>` |

Body: `{"install":["imagick"],"remove":[],"enable":[{"name":"xdebug","sapi":"cli"}],"disable":[{"name":"xdebug","sapi":"fpm"}]}`.
- `install` runs `apt-get install php<v>-<ext>`. `remove` runs `apt-get remove`, and is
  `reversible:false`.
- `enable` runs `phpenmod -v <v> -s <fpm|cli> <ext>`. `disable` runs
  `phpdismod -v <v> -s <sapi> <ext>`. These only touch symlinks in `conf.d`, which are
  snapshotted.
- Then: `php-fpm<v> -t` and `systemctl reload php<v>-fpm`. Verify that `php-fpm<v> -m`
  shows the expected set.
- **Warning:** xdebug enabled for `fpm` is flagged as a warning by the advisor rule
  `xdebug-in-fpm`.

## 5. php.ini drop-ins

The files are `/etc/php/<v>/fpm/conf.d/99-nexacloud.ini` and
`/etc/php/<v>/cli/conf.d/99-nexacloud.ini`, rendered whole from `managed_settings`.

`GET /api/php/{ver}/ini` returns:
```json
{"effective":{"memory_limit":"512M"},"managed":{"memory_limit":"512M"},"sources":{"memory_limit":"99-nexacloud.ini"}}
```
`effective` comes from `php-fpm<v> -i`. It reflects the FPM SAPI, not the pools.

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/php/{ver}/ini/preview` / `apply` | C (`restart`: reload php<v>-fpm) | – | `php:<v>` |

Body: `{"sapi":"fpm|cli|both","set":{"memory_limit":"512M"},"unset":["max_input_vars"]}`.

Allowlisted keys and validation (anything else returns `400 invalid`):

| Key | Regex / range |
|---|---|
| `memory_limit` | `^(-1|[1-9][0-9]{1,4}M|[1-9]G)$` |
| `upload_max_filesize`, `post_max_size` | `^[1-9][0-9]{0,4}[MG]$`, ≤ 2G. `post_max_size` must be ≥ `upload_max_filesize`. |
| `max_execution_time`, `max_input_time` | int 0–3600 |
| `max_input_vars` | int 1000–100000 |
| `date.timezone` | an entry in `/usr/share/zoneinfo` |
| `display_errors` | `Off` (the only value accepted for fpm) |
| `expose_php` | `Off` / `On` |
| `opcache.enable`, `opcache.validate_timestamps` | `0` / `1` |
| `opcache.memory_consumption` | int 64–1024 |
| `opcache.interned_strings_buffer` | int 8–128 |
| `opcache.max_accelerated_files` | int 2000–100000 |
| `opcache.revalidate_freq` | int 0–300 |
| `opcache.jit`, `opcache.jit_buffer_size` | `off|tracing|function` ; `^[0-9]{1,4}M$` |
| `realpath_cache_size` | `^[0-9]{1,5}[KM]$` |
| `realpath_cache_ttl` | int 0–86400 |
| `session.gc_maxlifetime` | int 60–604800 |

"Save & reload FPM" is the apply button.

## 6. CLI default

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/php/cli-default/preview` / `apply` | C (safe) | – | `php:<v>` |

Body: `{"version":"8.4"}`. The commands are:
- `update-alternatives --set php /usr/bin/php<v>`
- `update-alternatives --set phar /usr/bin/phar<v>`
- `update-alternatives --set phar.phar /usr/bin/phar.phar<v>`

Skip the last two if their targets are missing. The snapshot records the previous
`update-alternatives --query php` value, and rollback re-runs `--set` with it. This
change has no file diff. The plan's `data.previous` holds the old value.

Composer, cron and supervisor entries written by NexaCloud always use explicit
`/usr/bin/php<v>`, so changing the CLI default never changes how sites run.

## 7. Service install catalog

`GET /api/catalog` (P1):
```json
{"repos":[{"id":"ondrej-php","label":"ondrej/php","configured":true},{"id":"pgdg","label":"PGDG","configured":false},{"id":"nodesource","label":"NodeSource","configured":true}],
 "items":[{"id":"redis","name":"Redis","installed":true,"version":"7.4.1","active":true,"state":"running|stopped|not_installed|degraded|installing",
           "note":"Cache + queues · 212 MB used of 512 MB","available":true,"conflicts_with":[],"units":["redis-server"],"actions":["manage","restart","flush"]}]}
```
`state: degraded` is used for Supervisor when any program is `FATAL`/`BACKOFF`, or
stalled (projects-deploys.md §9.4). The note then reads "1 stalled".

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/catalog/{id}/install/preview` / `apply` | C, `reversible:false` (drop-ins revertible) | – | `apt` + item lock (below), heavy |
| POST | `/api/catalog/{id}/uninstall/preview` / `apply` | C, `reversible:false` | ✓ | same |
| POST | `/api/services/{unit}/start` / `stop` / `restart` | J | ✓ (restart is already sudo in Phase 0) | `service:<unit>` |
| POST | `/api/redis/flush` | J | ✓ | `redis` |

- The known-unit list from v0.1 is extended with `clamav-freshclam`, `certbot.timer` and
  `nc-frankenphp-*`.
- `POST /api/services/{unit}/restart` keeps its v0.1 response `{"ok":true}`. It also
  writes an audit row. The other actions return `{"job_id"}`.
- **Redis flush body:** `{"db":0|"all","confirm":"FLUSH"}`. It runs
  `redis-cli -n <db> FLUSHDB`, or `FLUSHALL` for `all`. The UI must list the queues
  that would be lost: the `queues:*` key count.

### 7.1 Items

The apt commands use the common prefix above. "Post" steps run in the same job.

| id | Packages / source | Item lock | Post-install (all rendered drop-ins snapshotted) |
|---|---|---|---|
| `redis` | `redis-server` | `redis` | Write the unit override and `/etc/nexacloud/redis.conf` (below), then `systemctl daemon-reload` and `systemctl restart redis-server`. Verify `redis-cli PING` = `PONG`. |
| `supervisor` | `supervisor` | `supervisor` | `systemctl enable --now supervisor` |
| `ufw` | `ufw` | `firewall` | Install only. Enabling it is §8 (lockout). |
| `fail2ban` | `fail2ban` | `fail2ban` | Write `/etc/fail2ban/jail.d/zz-nexacloud.local` (below), then `systemctl enable --now fail2ban` and `fail2ban-client reload`. Verify `fail2ban-client status sshd`. |
| `clamav` | `clamav clamav-freshclam` (**not** `clamav-daemon`: clamd needs about 1 GB RAM) | `clamav` | `systemctl enable --now clamav-freshclam`. Write `nexacloud-clamscan.service` and `nexacloud-clamscan.timer` (below). `systemctl enable --now nexacloud-clamscan.timer`. |
| `mysql` | `mysql-server` (distro 8.0). MySQL 8.4 LTS from repo.mysql.com is **Q11**. | `db-server:mysql` | `systemctl enable --now mysql`. Write `zz-nexacloud.cnf` (advisor defaults: bind 127.0.0.1, slow log). Provision `nexacloud_admin` (database-manager.md §2). |
| `mariadb` | `mariadb-server`. `conflicts_with:["mysql"]`: refused with 409 if `mysql-server` is installed. | `db-server:mysql` | Same as mysql, with the `mariadb.conf.d` path. |
| `postgresql` | Distro `postgresql`, or with `{"source":"pgdg","major":17}`: the PGDG repo (below), then `postgresql-17` | `db-server:postgres` | `systemctl enable --now postgresql`. Write `conf.d/zz-nexacloud.conf` (`listen_addresses = 'localhost'`). Provision the admin role in P3. |
| `node` | NodeSource repo (below), `nodejs`. Body `{"major":22}`, one of 20, 22, 24. | `node` | Verify `node -v`, `npm -v`. |
| `composer` | Not apt (below) | `composer` | Write `/usr/local/bin/composer`, `nexacloud-composer-update.service` and `nexacloud-composer-update.timer` (weekly). |
| `certbot` | `certbot` | `certbot` | `systemctl enable --now certbot.timer`. ACME dir and hook as in sites.md §5. |
| `rclone` | `rclone` (distro) | – | Verify `rclone version` ≥ 1.60. If lower, install from `https://downloads.rclone.org/v<ver>/rclone-v<ver>-linux-amd64.zip` with a SHA256SUMS check (backups.md §2). |
| `unattended-upgrades` | `unattended-upgrades` | `apt` | Drop-in in advisor.md |
| `tailscale` | Repo `https://pkgs.tailscale.com/stable/ubuntu/<codename>` (`.noarmor.gpg` key → `/usr/share/keyrings/tailscale-archive-keyring.gpg`), `tailscale` | `tailscale` | `systemctl enable --now tailscaled`. Joining the tailnet is backups.md §2.3. |
| `mongodb`, `meilisearch` | – | – | `available:false`, `reason:"not planned"` (Q9) |

**Redis.**
`/etc/systemd/system/redis-server.service.d/zz-nexacloud.conf`:
```ini
[Service]
ExecStart=
ExecStart=/usr/bin/redis-server /etc/nexacloud/redis.conf --supervised systemd --daemonize no
```
`/etc/nexacloud/redis.conf` (mode 0640, `root:redis`):
```
include /etc/redis/redis.conf
bind 127.0.0.1 -::1
protected-mode yes
maxmemory 512mb
maxmemory-policy volatile-lru
```
`volatile-lru` evicts only keys with a TTL, which are cache keys. Laravel queue keys have
no TTL, so they survive memory pressure. Check the Ubuntu unit's `ExecStart` at
implementation; it has been
`/usr/bin/redis-server /etc/redis/redis.conf --supervised systemd --daemonize no`.

**Fail2ban** `/etc/fail2ban/jail.d/zz-nexacloud.local`:
```ini
# Managed by NexaCloud
[DEFAULT]
bantime  = 1h
findtime = 10m
maxretry = 5
ignoreip = 127.0.0.1/8 ::1

[sshd]
enabled = true
backend = systemd

[recidive]
enabled  = true
bantime  = 1w
findtime = 1d
```
- `ignoreip` additionally lists the current admin IP, from the request.
- Fail2ban on Ubuntu 24.04 needs package ≥ `1.0.2-3ubuntu0.1`, because of a Python 3.12
  fix. Check that version in preflight.

**ClamAV scan units.**
`/etc/systemd/system/nexacloud-clamscan.service`:
```ini
[Unit]
Description=NexaCloud nightly malware scan of /var/www
[Service]
Type=oneshot
Nice=19
IOSchedulingClass=idle
MemoryMax=1200M
ExecStart=/usr/bin/clamscan -r -i --no-summary --max-filesize=50M --max-scansize=200M --exclude-dir=^/var/www/[^/]+/(releases/[^/]+/)?(node_modules|vendor|\.git)$ --log=/var/log/nexacloud/clamscan.log /var/www
SuccessExitStatus=1
```
`SuccessExitStatus=1` is there because exit 1 means "infected found". The agent parses
the log for `FOUND` lines and raises advisor rule `malware-found`.

`/etc/systemd/system/nexacloud-clamscan.timer`:
```ini
[Timer]
OnCalendar=*-*-* 04:30
RandomizedDelaySec=30m
Persistent=true
[Install]
WantedBy=timers.target
```

**PGDG repo.**
1. Key: `https://www.postgresql.org/media/keys/ACCC4CF8.asc`. The agent downloads it
   and stores it as `/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc`.
2. `/etc/apt/sources.list.d/pgdg.sources`:
   ```
   Types: deb
   URIs: https://apt.postgresql.org/pub/repos/apt
   Suites: <codename>-pgdg
   Components: main
   Signed-By: /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc
   ```
3. `apt-get update`.

**NodeSource.**
1. Key `https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key`, saved through
   `gpg --dearmor` to `/etc/apt/keyrings/nodesource.gpg`.
2. `/etc/apt/sources.list.d/nodesource.list`:
   `deb [signed-by=/etc/apt/keyrings/nodesource.gpg] https://deb.nodesource.com/node_<major>.x nodistro main`.
3. `apt-get update`, then `apt-get install nodejs`.
4. Verify the URL format at implementation.

**Composer.**
1. Download `https://getcomposer.org/installer` to `/run/nexacloud/tmp/composer-setup.php`.
2. Fetch the expected hash from `https://composer.github.io/installer.sig`.
3. Compare SHA-384. Abort on mismatch.
4. Run `php /run/nexacloud/tmp/composer-setup.php --install-dir=/usr/local/bin --filename=composer`.
5. The update timer runs `/usr/local/bin/composer self-update --2 --no-interaction`
   weekly (`OnCalendar=Sun 05:00`).

Composer runs as root only for `self-update`. Deploys run it as the site user.

## 8. Firewall (UFW)

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| GET | `/api/firewall` | – | – | – |
| POST | `/api/firewall/preview` / `apply` | C, **lockout** | ✓ | `firewall` |

`GET` returns:
```json
{"installed":true,"active":false,"default_incoming":"deny","rules":[{"n":1,"to":"22/tcp","action":"ALLOW","from":"Anywhere","v6":false}],"ssh_ports":[22],"panel_port":8443}
```
- `ufw status numbered` and `ufw status verbose` are parsed.
- `ssh_ports` comes from `sshd -T | grep ^port`.

**P1 body** (enable with defaults, the only shape in P1): `{"preset":"defaults"}`.

The plan's commands, in order:
1. `ufw default deny incoming`
2. `ufw default allow outgoing`
3. `ufw allow <ssh_port>/tcp`, for each SSH port
4. `ufw allow 80/tcp`
5. `ufw allow 443/tcp`
6. `ufw allow <panel_port>/tcp`
7. `ufw --force enable`

Snapshot: copy `/etc/ufw/user.rules`, `/etc/ufw/user6.rules` and `/etc/default/ufw`,
and record the active state. Rollback restores the files, then runs `ufw reload`, or
`ufw disable` if UFW was inactive.

The confirm-or-revert flow is in README §1.8.

**P3** extends the body to
`{"rules":[{"action":"allow|deny|limit","port":"^[0-9]{1,5}(:[0-9]{1,5})?$","proto":"tcp|udp","from":"<CIDR>|any","comment":"…"}],"delete":[n]}`.
It also adds:
- SSH: `POST /api/ssh/{preview,apply}`, lockout tier. It writes
  `/etc/ssh/sshd_config.d/99-nexacloud.conf` with `PasswordAuthentication no` and/or
  `Port`. It is refused unless an `authorized_keys` with at least one key exists for a
  user who can log in. Verify `sshd -t`, then `systemctl reload ssh`.
- `POST /api/panel/allowlist/{preview,apply}`, lockout tier: a UFW rule restricting
  `panel_port`, plus nginx `allow/deny` on the panel vhost.

Ubuntu 22.04's `sshd_config` includes `sshd_config.d/*.conf` **first**, so the first
value wins. A file named `99-…` may therefore be overridden by an earlier
`50-cloud-init.conf`. The plan detects this with `sshd -T` after writing, and fails the
verify step if the effective value differs.

## 9. Open questions
- **Q11.** MySQL 8.4 LTS (as in the mockup) needs the repo.mysql.com apt repo and the
  `mysql-apt-config` dance. Is distro 8.0 acceptable for P1?
- **Q12.** Should removing an unused `www` pool be offered? It means editing a vendor
  file. Proposal: never edit it; the advisor reports its idle cost only.
