# FrankenPHP runtime (Phase 3)

Conventions are in [README.md](README.md). FrankenPHP is an **alternative runtime per
site**:
- A per-site systemd unit listens on a loopback port behind nginx.
- Classic mode is the default.
- Worker mode is offered only for Laravel with Octane.
- The site's PHP-FPM pool is **kept** (ondemand, so idle cost is about 0) for one-click
  switch back.

Facts below marked *(verified)* were checked against frankenphp.dev and the
laravel.com Octane docs on 2026-11-13. Facts marked *(verify)* must be confirmed at
implementation.

**Sudo baseline.** Every class C or D `apply` needs sudo (README §1.9, following the
Phase 0 precedent). On preview/apply rows, the `Sudo` column marks whether the
**preview** also needs sudo. On J and S rows, it marks the endpoint itself.

## 1. Install
| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| GET | `/api/frankenphp` | – | – | – |
| POST | `/api/frankenphp/install/preview` / `apply` | C, `reversible:false` | – | `frankenphp`, heavy |

**GET** returns:
```json
{"installed":true,"version":"1.13.5","php_version":"8.5.2","source":"static|deb","binary":"/usr/local/bin/frankenphp",
 "extensions":["bcmath","ctype","curl","…"],"latest":"1.13.5","sites":[{"site_id":"api.nexadev.io","mode":"classic|worker","port":9101,"active":true}]}
```

**Install body:** `{"source":"static","version":"latest"}`.

**`source: static`** (default):
- **Download.** The asset is `frankenphp-linux-x86_64` *(verified asset name)* from
  `https://github.com/php/frankenphp/releases/download/v<ver>/frankenphp-linux-x86_64`.
- **Checksum.** No checksum file is published *(verified)*. Fetch
  `GET https://api.github.com/repos/php/frankenphp/releases/tags/v<ver>` and compare the
  download's sha256 with the asset's `digest` field (`sha256:…`). Abort on mismatch.
- **Placement.**
  - Binary: `/usr/local/lib/nexacloud/frankenphp/<ver>/frankenphp` (0755).
  - Symlink: `/usr/local/bin/frankenphp`.
  - Old versions are kept for rollback; the 2 newest stay.
- **Embedded PHP.** The binary embeds PHP. The current release embeds 8.5 *(verified)*,
  with a fixed extension set.
- **Verify.** Run `frankenphp version` *(verify subcommand)* and `frankenphp php-cli -r 'echo PHP_VERSION;'`.
  Extensions come from `frankenphp php-cli -r 'echo implode(",", get_loaded_extensions());'`.
  This uses only `php-cli <script>`/`-r`, so it does not depend on whether `-m` works
  *(verify `-r`)*.

**`source: deb`** (when the site's PHP minor must match, e.g. 8.3):
- The third-party apt repo is `https://pkg.henderkes.com/api/packages/<82|83|84|85>/debian`,
  suite `php-zts`, component `main`, key `…/repository.key`, package `frankenphp`
  *(verified on frankenphp.dev)*.
- Its ini lives under `/etc/php-zts/`, and extensions come as separate packages *(verify
  names)*.
- Only one deb flavour can be installed per server. The UI says so (Q20).

## 2. Model (per site)
Stored in `sites` under `runtime: "frankenphp"` and `frankenphp`:
```json
{"mode":"classic|worker","port":9101,"num_threads":4,"workers":2,"max_requests":500,
 "memory_max":"1024M","cpu_quota":"150%","unit":"nc-frankenphp-api","fpm_pool_kept":true,"switched_at":"…","previous_runtime":"fpm"}
```

| Field | Rule |
|---|---|
| `port` | Allocated from 9100–9199. Must not be listening (`ss -Hltn 'sport = :<p>'`). Unique in SQLite. |
| `num_threads` | 2–32. Default `2 × vCPU` = 4. |
| `workers` | 1–16. Default `vCPU` = 2. `num_threads` must be strictly greater than `workers` *(verified)*. |
| `max_requests` | 0–100000 |
| `memory_max` | `^[0-9]{3,4}M$`, 256M–4096M |
| `cpu_quota` | `^[0-9]{2,3}%$`, 10%–400% |

## 3. Files
Directory: `/etc/nexacloud/frankenphp/<slug>/` (0750, `root:<site group>`).

**`Caddyfile` (classic mode):**
```caddyfile
# Managed by NexaCloud — site api.nexadev.io
{
	admin off
	auto_https off
	persist_config off
	frankenphp {
		num_threads 4
		php_ini memory_limit 256M
		php_ini upload_max_filesize 64M
		php_ini post_max_size 64M
		php_ini opcache.memory_consumption 256
	}
	servers {
		trusted_proxies static 127.0.0.1/32 ::1/128
	}
	log {
		output file /var/log/nexacloud/frankenphp/<slug>.log {
			roll_size 50MiB
			roll_keep 5
		}
		format json
		level WARN
	}
}

http://127.0.0.1:9101 {
	root * /var/www/api.nexadev.io/current/public
	php_server
}
```
- `admin off` avoids admin-port collisions between sites. It means reloads become
  restarts (§7).
- `php_ini` lines are generated from the site's effective `99-nexacloud.ini` plus pool
  `php_admin_value`s, using the same allowlist as php-services.md §5. This keeps
  behaviour identical to FPM.
- Gzip and headers stay in nginx, so there is no `encode` directive.

**`env`** (0640), used as `EnvironmentFile=`:
```
HOME=/var/lib/nexacloud/home/nc_api
XDG_DATA_HOME=/var/lib/nexacloud/frankenphp/<slug>/data
XDG_CONFIG_HOME=/var/lib/nexacloud/frankenphp/<slug>/config
PHP_INI_SCAN_DIR=/etc/nexacloud/frankenphp/<slug>/php.d
```
- `PHP_INI_SCAN_DIR` is honoured *(verified)*.
- `php.d/` holds `99-nexacloud.ini`, a copy of the FPM drop-in. It is needed because a
  static binary also reads `/etc/frankenphp/php.d`. Pinning the scan dir keeps sites
  isolated.

**Worker mode** (Laravel + Octane only). Octane owns its Caddyfile. NexaCloud runs
Octane's command instead of `frankenphp run`:
```
/usr/bin/php<v> artisan octane:frankenphp --host=127.0.0.1 --port=9101 --admin-port=10101 \
    --workers=2 --max-requests=500 --log-level=warn
```
- The command and flags are *(verified)* on frankenphp.dev/docs/laravel. `octane:start
  --server=frankenphp` is equivalent.
- The admin port is `port + 1000` on loopback, unique per site, because Octane uses the
  admin API for reloads.
- Octane needs a binary named `frankenphp` on `PATH` *(verified)*. The unit sets
  `PATH=/usr/local/bin:/usr/bin:/bin`.
- `php_ini` settings go through `PHP_INI_SCAN_DIR` as above.
- The worker-file location is chosen by Octane, so NexaCloud never writes one.

## 4. Unit `/etc/systemd/system/nc-frankenphp-<slug>.service`
```ini
[Unit]
Description=FrankenPHP · api.nexadev.io (managed by NexaCloud)
After=network.target
StartLimitIntervalSec=60
StartLimitBurst=5

[Service]
Type=simple
User=nc_api
Group=nc_api
WorkingDirectory=/var/www/api.nexadev.io/current
EnvironmentFile=/etc/nexacloud/frankenphp/api/env
Environment=PATH=/usr/local/bin:/usr/bin:/bin
ExecStart=/usr/local/bin/frankenphp run --config /etc/nexacloud/frankenphp/api/Caddyfile --adapter caddyfile
# worker mode: ExecStart=/usr/bin/php8.4 artisan octane:frankenphp --host=127.0.0.1 --port=9101 --admin-port=10101 --workers=2 --max-requests=500 --log-level=warn
Restart=on-failure
RestartSec=2
TimeoutStopSec=30
MemoryMax=1024M
CPUQuota=150%
TasksMax=512
LimitNOFILE=65535
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=full
ProtectHome=yes
ReadWritePaths=/var/www/api.nexadev.io /var/log/nexacloud/frankenphp /var/lib/nexacloud/frankenphp/api

[Install]
WantedBy=multi-user.target
```
- `Type=simple` is used because `Type=notify` support is unverified. Readiness is
  checked by the agent's probe (§5), not by systemd.
- `--config` *(verified)*. `--adapter caddyfile` is Caddy-standard *(verify)*.

## 5. Preflight
`POST /api/sites/{id}/runtime/preflight` (J, P3, lock `site:{id}`).
Body: `{"to":"frankenphp","mode":"classic|worker"}`.

The result:
```json
{"ok":false,"blocking":["missing extension: imagick"],
 "php":{"site":"8.3","frankenphp":"8.5","status":"newer|same|older"},
 "extensions":{"missing":["imagick"],"extra":["swoole"],"site":["…"],"frankenphp":["…"]},
 "ini_diff":[{"key":"memory_limit","fpm":"512M","frankenphp":"512M"}],
 "octane":{"required":true,"installed":true,"version":"2.6.1"},
 "warnings":["PHP goes from 8.3 to 8.5: run the PHP compatibility scan (php-services §3.3)",
             "Laravel TrustProxies must trust 127.0.0.1 or REMOTE_ADDR will be the proxy (verify)"],
 "compat_scan":{"status":"compatible","job_id":null}}
```

Each check, and whether it blocks:

| Check | How | Blocks? |
|---|---|---|
| Extensions | FPM's loaded extensions (`php-fpm<v> -m`) minus FrankenPHP's (§1). The `missing` list. | **Yes**, if not empty. |
| PHP version | FrankenPHP is older than the site's version | **Yes** |
| PHP version | FrankenPHP is newer than the site's version | No, but it auto-runs the compat scan with `target = frankenphp minor`. Errors block. |
| Worker mode: Octane | `laravel/octane` in `composer.lock` and `config/octane.php` exists | **Yes**, if missing (`precondition_failed`) |
| Writable paths | Checked as the site user | **Yes**, if not writable |
| Memory budget | `memory_max` + other runtimes ≤ the FPM budget (advisor.md §1.2) | No, warning only |

## 6. nginx proxy template (variant of sites.md §7.7)
The HEAD, ACME, security headers, gzip and maintenance lines are unchanged:
```nginx
    root {{root}};
    location ^~ /build/ { expires 30d; access_log off; try_files $uri =404; }      # static_cache: nginx serves built assets
    location ~ /\.(?!well-known).* { deny all; }                                     # block_hidden
    location / {
        proxy_pass http://127.0.0.1:{{port}};
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-Host $host;
        proxy_set_header Connection "";
        proxy_read_timeout {{request_timeout}}s;
        proxy_next_upstream error timeout;
        proxy_intercept_errors off;
    }
    include {{custom}};
```

## 7. Switch and switch back
| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/sites/{id}/runtime/preview` / `apply` | C (restart tier: starts or stops the unit, reloads nginx) | – | `site:{id}`, `nginx`, `frankenphp`, `php:<v>` |
| POST | `/api/sites/{id}/runtime/limits/preview` / `apply` | C (restart: the unit) | – | `site:{id}` |

**Body:** `{"to":"frankenphp|fpm","mode":"classic|worker","num_threads":4,"workers":2,"memory_max":"1024M"}`.
The preview fails with `422 precondition_failed` if the latest preflight is older than
1 h or not `ok`. The UI runs preflight first.

**FPM → FrankenPHP:**
1. Allocate a port. Write the Caddyfile or Octane settings, `env`, `php.d/` and the unit.
   Run `systemctl daemon-reload`, then `systemctl enable --now nc-frankenphp-<slug>`.
2. **Probe the upstream.** `GET http://127.0.0.1:<port><health_path>` with
   `Host: <primary>` and `X-Forwarded-Proto: https`. It must return the same status
   class as the FPM probe taken just before the switch. 10 attempts, 1 s apart.
3. **Swap.** Re-render the vhost with the §6 template through the nginx write path
   (sites.md §9), which includes the public probe.
4. **Keep FPM.** Leave the FPM pool file and socket in place (`fpm_pool_kept: true`).
5. **Wire up deploys.** Supervisor workers and cron are unchanged. The deploy `reload`
   step becomes `systemctl restart nc-frankenphp-<slug>`, which has a brief gap of about
   1–2 s. For worker mode it is `php artisan octane:reload` against the admin port *(verify)*.
6. **Auto-revert.** A failure at 2 or 3 reverts every file, stops and disables the unit,
   and restores the vhost.

**FrankenPHP → FPM** (switch back, same endpoint with `to: "fpm"`):
1. Probe the FPM pool (php-services.md §3.2). If the pool was removed, re-create it
   first.
2. Re-render the FPM vhost. Run `nginx -t`, reload, and probe.
3. `systemctl disable --now nc-frankenphp-<slug>`. Keep its files so the next switch is
   instant. The port allocation is kept for 30 days.

**Upgrade the binary:** `POST /api/frankenphp/install/{preview,apply}` with a new
`version`. For each FrankenPHP site, the job restarts the unit and probes it. On a probe
failure it re-points the symlink to the previous version and restarts again.

## 8. Resource limits and budget
- **Defaults** for 2 vCPU / 8 GB:

  | Setting | Value |
  |---|---|
  | `MemoryMax` | 1024M |
  | `CPUQuota` | 150% |
  | `num_threads` | 4 |
  | workers | 2 (worker mode) |
  | `max_requests` | 500 |
  | `TasksMax` | 512 |
- **Budget.** Each FrankenPHP site's `memory_max` is subtracted from the advisor FPM
  budget (advisor.md §1.2). The advisor rule `fpm-ondemand` therefore counts both.
- **Monitoring.** `GET /api/frankenphp` sites show `MemoryCurrent` and `CPUUsageNSec`
  from `systemctl show nc-frankenphp-<slug> -p MemoryCurrent,CPUUsageNSec`.

## 9. Open questions
- **Q20.** Static binary vs deb packages. The static binary pins one PHP minor for every
  FrankenPHP site. The deb repo is third-party, from the FrankenPHP maintainers, and
  installs one flavour per server. Proposal: static by default, deb as an opt-in per
  server.
- **Q21.** Should worker mode also support non-Octane apps (a custom worker script)? Not
  per the roadmap.
