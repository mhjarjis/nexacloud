# Backups and storage (Phase 1)

Conventions are in [README.md](README.md). The v0.1 endpoints `GET /api/backups`,
`GET /api/backups/download` and `POST /api/databases/{engine}/{name}/backup` stay
unchanged. The v0.1 local layout `{backup_dir}/{engine}/{name}/{name}-{YYYYmmdd-HHMMSS}.sql.gz`
is kept, and new artifacts follow the same pattern.

**Sudo baseline.** Every class C or D `apply` needs sudo (README §1.9, following the
Phase 0 precedent). On preview/apply rows, the `Sudo` column marks whether the
**preview** also needs sudo. On J and S rows, it marks the endpoint itself.

## 1. Model (SQLite)

| Table | Columns (main) |
|---|---|
| `backup_destinations` | `id`, `name`, `type` (`local\|sftp\|s3\|webdav`), `preset`, `config` (JSON, non-secret), `secret_ref`, `crypt` (bool), `crypt_ref`, `root_path`, `state` (`ok\|error\|untested`), `last_test_at`, `last_error`, `created_at` |
| `backup_plans` | `id`, `name`, `trigger` (`schedule\|pre_deploy\|manual`), `schedule` (cron string), `tz`, `sources` (JSON), `destination_ids` (JSON), `retention` (JSON), `enabled`, `next_run_at`, `builtin` (bool) |
| `backup_runs` | `id`, `plan_id`, `job_id`, `trigger` (`schedule\|manual\|pre_deploy\|run_all`), `status`, `started_at`, `finished_at`, `bytes`, `manifest` (JSON), `error` |
| `backup_artifacts` | `id`, `run_id`, `kind` (`db\|files\|config`), `target` (`db:mysql/crm_prod`, `site:shop.example.com`, `server`), `rel_path`, `bytes`, `sha256`, `created_at`, `label` (e.g. `Before deploy #148`) |
| `backup_copies` | `artifact_id`, `destination_id`, `remote_path`, `uploaded_at`, `verified_at`, `deleted_at` |

The **local disk** destination (`id: 1`, `type: local`, `root_path` = `backup_dir`)
always exists and cannot be deleted. Restore points are artifacts. The "where" column
("Local + S3") comes from `backup_copies`.

**Built-in plans,** created on first start (`builtin:true`). They can be edited but not
deleted.

| Name | Sources | Schedule | Destinations | Retention |
|---|---|---|---|---|
| All databases | `[{"type":"databases","engine":"*","names":"*"}]` | `0 3 * * *` | local | `{"daily":7,"weekly":4,"monthly":3}` |
| Before every deploy | `[{"type":"databases","of":"deploying_project"}]` | `trigger: pre_deploy` | local | `{"days":7}` |
| Server config | `[{"type":"server_config"}]` | `30 3 * * *` | local | `{"days":30}` |

## 2. Destinations (rclone)

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| GET | `/api/backup/destinations` | – | – | – |
| POST | `/api/backup/destinations` | S | ✓ (sets secrets) | – |
| PUT | `/api/backup/destinations/{id}` | S | ✓ if `secrets` or `crypt` in body, else – | `dest:{id}` (sync check: `409 resource_busy` while a run uses it) |
| DELETE | `/api/backup/destinations/{id}` | S | ✓ | `dest:{id}`. Body `{"confirm":"<name>","delete_remote":false}`. `delete_remote:true` makes it a J (`rclone purge`). |
| POST | `/api/backup/destinations/{id}/test` | J | – | `dest:{id}` |
| GET | `/api/backup/destinations/{id}/recovery-kit` | – (audited) | ✓ | – |
| POST | `/api/backup/destinations/hostkey` | – (read-only probe) | – | – |

The `rclone` catalog item must be installed, else `424 not_installed`.

**List item:**
```json
{"id":2,"name":"Synology (Tailscale)","type":"sftp","preset":"synology_tailscale","state":"ok",
 "note":"nas.tailnet-1234.ts.net:/NetBackup/nexacloud · 41.2 GB","crypt":true,
 "config":{"host":"nas.tailnet-1234.ts.net","port":22,"user":"nexacloud","path":"/NetBackup/nexacloud","host_key_fingerprint":"SHA256:…"},
 "last_test_at":"…","last_error":null,"used_by_plans":[1,3]}
```

**Create body.** Secret fields are write-only and never returned.
```json
{"name":"Synology (Tailscale)","preset":"synology_tailscale","type":"sftp",
 "config":{"host":"nas.tailnet-1234.ts.net","port":22,"user":"nexacloud","path":"/NetBackup/nexacloud","host_key_fingerprint":"SHA256:…"},
 "secrets":{"auth":"key"},
 "crypt":true}
```

### 2.1 Types, presets and validation
| type / preset | config fields | secrets | rclone backend |
|---|---|---|---|
| `sftp` / `generic`, `synology_tailscale`, `synology_ddns` | `host` (HOSTNAME or IP or Tailscale `100.64.0.0/10` IP), `port` 1–65535, `user` `^[A-Za-z0-9._-]{1,64}$`, `path` `^/[^\x00\n]{0,255}$`, `host_key_fingerprint` | `auth: "key"` (default: the agent generates an ed25519 key) or `{"auth":"password","password":"…"}` | `type = sftp`, `key_file`, `known_hosts_file`, `shell_type = none` (Synology), `md5sum_command = none`, `sha1sum_command = none` |
| `s3` / `aws` | `region` `^[a-z]{2}-[a-z]+-[0-9]$`, `bucket` `^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$`, `path` prefix | `access_key_id`, `secret_access_key` | `provider = AWS` |
| `s3` / `synology_c2` | `endpoint` (e.g. `https://eu-002.s3.synologyc2.net`), `region`, `bucket` | same | `provider = Synology` (verify the provider name against the installed rclone version; fall back to `Other`) |
| `s3` / `minio` (e.g. MinIO in Synology Container Manager over Tailscale) | `endpoint` `^https?://[^\s/]+(:[0-9]+)?$`, `bucket` | same | `provider = Minio` |
| `s3` / `b2` (mockup "Backblaze B2") | `endpoint` `https://s3.<region>.backblazeb2.com`, `bucket` | key id / application key | `provider = Other` |
| `s3` / `other` | `endpoint`, `region?`, `bucket` | same | `provider = Other` |
| `webdav` / `generic`, `nextcloud` | `url` `^https://…`, `vendor` | `user`, `password` | `type = webdav` |

Common config: `bwlimit` (`^[0-9]{1,5}[KM]$`, optional), `transfers` (1–8, default 2).

Secrets are stored in `/etc/nexacloud/secrets/rclone/<id>.json` (0600):
```json
{"secret_access_key":"…"}
```
For key auth, the key pair is `/etc/nexacloud/secrets/rclone/<id>_ed25519` and `.pub`.
It is generated with
`ssh-keygen -t ed25519 -N "" -C "nexacloud-backup@<hostname>" -f …`. Create and GET
return `public_key` so the user can add it on the NAS.

### 2.2 Rendering, crypt and recovery kit
At job start, the agent renders `/run/nexacloud/rclone/<job_id>.conf` (0600) and deletes
it at job end. Passwords are obscured by piping them to `rclone obscure -` on stdin.

Example for an sftp destination with crypt:
```ini
[d2]
type = sftp
host = nas.tailnet-1234.ts.net
port = 22
user = nexacloud
key_file = /etc/nexacloud/secrets/rclone/2_ed25519
known_hosts_file = /etc/nexacloud/secrets/rclone/2_known_hosts
shell_type = none
md5sum_command = none
sha1sum_command = none

[d2c]
type = crypt
remote = d2:/NetBackup/nexacloud/<hostname>
filename_encryption = standard
directory_name_encryption = true
password = <obscured>
password2 = <obscured>
```
- **Crypt keys.** Destination create and update are class S, so R1 does not apply, but
  they follow the same once-only rule. `crypt:true` generates two 32-byte random passwords (base64url),
  stored in `/etc/nexacloud/secrets/backup-crypt/<id>.json`. They **cannot be
  recovered** without the kit.
- **Upload target.** Jobs always upload to `d<id>c:` (crypt) or `d<id>:<path>/<hostname>`
  (plain).
- **UI copy.** "encrypted with rclone crypt (XSalsa20-Poly1305)". Not "AES-256" (README
  Q6).
- **Recovery kit.** A `text/plain` attachment `nexacloud-recovery-<name>.txt` holding an
  rclone.conf snippet with the **plain** passwords and restore instructions:
  `rclone copy d2c:… ./`. It is audited as `backup.recovery_kit.download`.
- **Kit reminder.** The UI nags until the kit has been downloaded at least once:
  `kit_downloaded_at` on the destination.

### 2.3 Synology specifics
- **Tailscale route** (`synology_tailscale`).
  - Requires the `tailscale` catalog item (php-services.md §7.1).
  - Joining the tailnet: `POST /api/tailscale/up` (J, ✓ sudo, lock `tailscale`). Body
    `{"auth_key":"tskey-…"}`.
  - The key is written to `/run/nexacloud/tmp/ts-<uuid>` (0600) and passed as
    `tailscale up --auth-key=file:/run/nexacloud/tmp/ts-<uuid> --hostname=<hostname>`.
    Verify the `file:` support in the installed tailscale version. Delete the file
    afterwards.
  - `GET /api/tailscale` returns `{"installed","up","self_ip","peers":[{"name","ip","online"}]}`
    for the host picker.
- **DDNS route** (`synology_ddns`). The host is e.g. `nas.synology.me`. The UI warns that
  SFTP is exposed to the internet and recommends key auth plus a non-22 port.
- **Synology setup checklist,** returned as `preset_help[]` for the UI:
  1. Enable SFTP in DSM: Control Panel → File Services → FTP → SFTP.
  2. Create user `nexacloud` with write access to the shared folder.
  3. Enable user home and add the public key to `~/.ssh/authorized_keys`.
  4. Note that SFTP paths are share-relative, e.g. `/NetBackup/nexacloud`.
- **Host key pinning.** `POST /api/backup/destinations/hostkey` with `{"host","port"}`
  runs `ssh-keyscan -T 5 -p <port> -t ed25519,ecdsa,rsa <host>` and returns
  `{"keys":[{"type","fingerprint_sha256"}]}`. The user confirms one. The confirmed
  fingerprint is stored in `config.host_key_fingerprint`. The full line goes to
  `/etc/nexacloud/secrets/rclone/<id>_known_hosts`.

### 2.4 Test connection (J)
The test runs these steps with a 60 s timeout:
1. `rclone --config <conf> mkdir <remote>:`
2. `rclone --config <conf> rcat <remote>:.nexacloud-test` (10 KiB random data on stdin)
3. `rclone --config <conf> cat <remote>:.nexacloud-test` and compare
4. `rclone --config <conf> deletefile <remote>:.nexacloud-test`
5. `rclone --config <conf> about <remote>: --json` (optional; S3 does not support it)

The result is `{"ok":true,"latency_ms":120,"free_bytes":…|null}`, and the destination's
`state` is set from it.

## 3. Schedules

- **Spec:** standard 5-field cron, `minute hour day-of-month month day-of-week`.
  - Fields allow `*`, numbers, `a-b`, lists `a,b`, and steps `*/n` or `a-b/n`.
    Day-of-week is `0-7` (0 and 7 are Sunday) or `mon..sun`.
  - Per-field regex: `^(\*|[0-9]{1,2}(-[0-9]{1,2})?)(/[0-9]{1,2})?(,(\*|[0-9]{1,2}(-[0-9]{1,2})?)(/[0-9]{1,2})?)*$`.
    Names are lowercased and mapped to numbers first.
  - Ranges are checked per field.
  - Shortcuts: `@daily` = `0 3 * * *`, `@weekly` = `0 4 * * 0`, `@hourly` = `0 * * * *`.
  - The minimum interval between two runs is 15 min. Validate by computing the next 10
    fire times.
- **`tz`:** `"server"` (default, `timedatectl show -p Timezone --value`) or an IANA name
  from `/usr/share/zoneinfo`.
- **Internal scheduler.**
  - A tokio task ticks every 30 s.
  - When `next_run_at <= now`, it enqueues the plan's run job and then computes the next
    `next_run_at`. The value is persisted, so no run is lost on restart.
  - **Catch-up:** at startup, plans whose `next_run_at` passed while the agent was down
    run once if they are less than 6 h late. Otherwise they are recorded as `missed` in
    `backup_runs`.
- `GET /api/backup/plans` returns each plan plus `next_runs` (the next 3 fire times) and
  a human label (`"Daily 03:00"`, `"Sunday 04:00"`) computed server-side.

## 4. What is backed up (sources)

| Source JSON | Produces | Command (argv; secrets via defaults file) |
|---|---|---|
| `{"type":"databases","engine":"mysql","names":["crm_prod"]\|"*"}` | `mysql/<db>/<db>-<ts>.sql.gz` | `mysqldump --defaults-extra-file=/etc/nexacloud/secrets/mysql-admin.cnf --single-transaction --quick --routines --triggers --events --hex-blob --default-character-set=utf8mb4 [--set-gtid-purged=OFF (MySQL only)] --databases <db>`, piped through in-process gzip level 6 |
| `{"type":"databases","engine":"postgres",…}` | `postgres/<db>/<db>-<ts>.pgdump` | `runuser -u postgres -- pg_dump -Fc -Z 6 <db>` (stdout) |
| `{"type":"site_files","site_ids":["shop.example.com"]\|"*","paths":["storage/app","public/uploads",".env"]}` | `files/<site>/<site>-<ts>.tar.gz` | `tar --create --gzip --file=- --directory=<base_or_shared> --exclude=node_modules --exclude=vendor --exclude=storage/framework/cache --exclude=storage/logs -- <paths…>` |
| `{"type":"server_config"}` | `config/server-<ts>.tar.gz` | `tar --create --gzip --file=- --ignore-failed-read -- /etc/nginx /etc/php /etc/supervisor /etc/cron.d /etc/letsencrypt /etc/mysql /etc/postgresql /etc/redis /etc/fail2ban/jail.d /etc/ufw /etc/ssh/sshd_config.d /etc/nexacloud /var/spool/cron/crontabs`, plus `state.db` captured with SQLite `VACUUM INTO '/run/nexacloud/tmp/state-<ts>.db'` |

- **Site files.** `paths` defaults to `storage/app`, `public/uploads` (if present) and
  `.env`. In releases mode, paths resolve under `shared/`. Each path must canonicalize
  inside the site base.
- **Server config.** This archive contains private keys and secrets. Uploading it to a
  destination with `crypt:false` is refused with `400 invalid`. It stays local only.
- **Mockup "Includes" text.** It is computed: e.g. "5 MySQL + 1 PostgreSQL",
  "storage/app, uploads, .env", "nginx, php.ini, supervisor, crontab".

### 4.1 Run job
| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/backup/plans/{id}/run` | J, heavy | – | `backup-plan:{id}`, `db:<engine>/<name>` for each DB, `site:<id>` (read; files), `dest:<id>` for each destination |
| POST | `/api/backup/run-all` | J (enqueues one job per enabled schedule plan; returns `{"job_ids":[…]}`) | – | per job |

`GET /api/backup/runs?plan_id=&limit=&cursor=` (P1) returns paginated runs:
`{"id","plan_id","plan_name","trigger","status","started_at","finished_at","bytes","artifacts":n,"destinations":{"2":"ok"},"job_id","error"}`.
The plans table's "Last run" column comes from each plan's newest run.

Plan CRUD is class S:
- `POST /api/backup/plans`
- `PUT /api/backup/plans/{id}`
- `DELETE /api/backup/plans/{id}`, with `{"confirm":"<name>"}`; built-in plans cannot
  be deleted.

**Job steps:**
1. Dump or archive each source into `{backup_dir}/…`. Each artifact file is mode 0600,
   and its sha256 is computed while writing.
2. Local integrity check (§6.1).
3. Upload to each destination:
   `rclone --config <conf> copyto <local> <remote>:<rel_path> --transfers <n> [--bwlimit <x>] --stats 10s --stats-one-line -v`.
   Progress lines go to the job log.
4. Remote check (§6.1).
5. Retention pass for this plan on each destination (§5).
6. Write the manifest `{backup_dir}/runs/<run_id>.json`
   (`{run_id, plan, host, created_at, artifacts:[{rel_path,bytes,sha256}]}`). Upload it
   next to the artifacts.

The job `result` is `{"run_id","bytes","artifacts":n,"destinations":{"2":"ok","3":"failed: …"}}`.

If a destination fails, the run is `failed`, but local artifacts and successful copies
are kept. The **advisor** freshness rule reads `backup_copies`.

**Pre-deploy trigger.** The deploy pipeline (projects-deploys.md §5) calls the same
runner synchronously with `trigger:"pre_deploy"`, `label:"Before deploy #<n>"`, and the
project's databases. It uses the same locks.

## 5. Retention (GFS)
`retention` is `{"daily":D,"weekly":W,"monthly":M}` (defaults 7/4/3, each 0–365) or
`{"days":N}` (flat).

Rules, per (plan, target, destination):
- **Daily.** For each of the last `D` calendar days with a backup, keep the **newest**
  artifact of that day.
- **Weekly.** For each of the last `W` ISO weeks, keep the newest artifact of that week.
- **Monthly.** For each of the last `M` months, keep the newest artifact of that month.
- **Union.** Keep the union of those three sets, plus anything less than 24 h old,
  plus anything referenced by an unfinished restore. Delete everything else.
- **Never delete the last copy.** The newest successful artifact per target is never
  deleted, even if `retention` would remove it.
- **Days are server-tz calendar days.**
- **Deletion commands:** local files are deleted with `unlink`. Remote copies are deleted
  with `rclone deletefile <remote>:<rel_path>`.
- **Records.** `backup_copies.deleted_at` is set. The `backup_artifacts` row is deleted
  once no copies remain.

A dry-run of retention is included in the plan GET as `retention_preview` (what the next
pass would delete). Changing `retention` via PUT is class S, but if it would delete more
than 0 artifacts immediately, the response includes `would_delete:n` and the next run
applies it.

## 6. Restore points, integrity and restore

### 6.1 Integrity
- **Local, after each dump:**
  - `.sql.gz`: stream-decompress fully in-process, and check that the last line
    contains `-- Dump completed`.
  - `.pgdump`: `pg_restore --list <file>` exits 0.
  - `.tar.gz`: `tar --list --gzip --file=<f>` exits 0.
  - Then compare the sha256 against the one recorded while writing.
- **Remote, after upload:**
  - plain remotes: `rclone check <local_file_dir> <remote>:<dir> --one-way --include <name>`
  - crypt remotes: `rclone cryptcheck <local_dir> <remote>c:<dir> --one-way --include <name>`
  - On success, set `backup_copies.verified_at`.
- **Weekly test restore.**
  - `POST /api/backup/points/{id}/verify` runs it on demand (J, locks
    `db:<engine>/nc_verify_<ts>`).
  - It also runs on schedule: built-in Sunday 05:00, newest artifact per DB.
  - It restores into the scratch DB `nc_verify_<YYYYmmddHHMM>`, counts tables, then
    drops it.
  - The result is `{"tables":84,"ok":true}`, stored in `backup_artifacts.verify`.

### 6.2 Restore points
`GET /api/backup/points?target=db:mysql/crm_prod|db:*|site:shop.example.com|server&cursor=…` (P1):
```json
{"items":[{"id":812,"target":"db:mysql/crm_prod","when":"2026-11-13T03:00:00Z","kind":"nightly|weekly|monthly|pre_deploy|manual","label":"Before deploy #148",
  "bytes":212000000,"where":["local","2"],"verified_at":"…","sha256":"…"}],"next_cursor":null}
```
`kind` is derived: an artifact kept *only* by its weekly or monthly slot shows as
`weekly` or `monthly`.

`POST /api/backup/points/{id}/download` (P1). Body `{"from":"local"|<dest_id>}`.
- Local artifacts stream directly, like v0.1 `/download`, with
  `Content-Disposition: attachment`.
- For a remote copy, a J first fetches it to `{backup_dir}/restore-cache/` with
  `rclone copyto` (decrypting via crypt). The job result has a `download_url`, which is
  only valid for that job.

### 6.3 Restore
| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/backup/points/{id}/restore/preview` / `apply` | C (DB server objects, not files) | ✓* (`mode: replace`) | copy: `db:<engine>/<copy>`. replace: `db:<engine>/<db>`, `site:<id>` for each site using it. heavy. |
| POST | `/api/backup/restores/{rid}/swap/preview` / `apply` | C | ✓ | `db:<engine>/<db>`, `db:<engine>/<copy>`, `site:<id>`… |
| POST | `/api/backup/restores/{rid}/discard` | J | – | `db:<engine>/<copy>` |

Body: `{"mode":"copy|replace","confirm":"crm_prod"}`. `confirm` is required for
`replace`.

**mode `copy`** ("Safe. Inspect it first, live site untouched"):
1. Fetch the artifact from local or a remote.
2. Name the copy `<db>_restore_<YYYYmmdd>`, appending `_2`, `_3`, … if taken. Truncate
   `<db>` so the name fits in 64 chars (postgres: 63).
3. Create it:
   - mysql: `CREATE DATABASE … CHARACTER SET utf8mb4`
   - postgres: `createdb -O <owner of original>`
4. Load the dump:
   - mysql: decompress, filter out `CREATE DATABASE`/`USE` lines (dumps use
     `--databases`), and pipe the stream to `mysql --defaults-extra-file=… <copy>` on
     stdin.
   - postgres: `pg_restore --no-owner --role=<owner> -d <copy>`.
5. Insert a `restores` row
   `{rid, point_id, db, copy, state:"ready", created_at}`. The UI then offers
   "Browse copy" (DB manager), "Swap into live", and "Discard".

**mode `replace`** ("Turns on maintenance mode, takes a safety dump of the current data,
then restores"):
1. Maintenance on for each site using the DB (projects-deploys.md §9.8).
2. Safety dump of live, as a `manual` artifact labelled `Safety dump before restore`.
3. mysql: `DROP DATABASE <db>; CREATE DATABASE <db> CHARACTER SET utf8mb4 COLLATE <orig collation>`.
   Then load the dump. Grants in `mysql.db` survive the drop and create.
   postgres: terminate backends, `DROP DATABASE`, `createdb -O <owner>`, `pg_restore`.
4. Maintenance off. Probe each site.
5. On failure in step 3, load the safety dump back, then maintenance off. The job
   reports `failed`.

**Swap** (copy → live, after inspection):
- mysql: dump the copy and run the same steps as `replace`. MySQL has no
  `RENAME DATABASE`, and `RENAME TABLE` across schemas breaks on triggers.
- postgres: maintenance on, then
  `SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname IN (…)`, then
  `ALTER DATABASE <db> RENAME TO <db>_old_<ts>` and
  `ALTER DATABASE <copy> RENAME TO <db>`, then maintenance off and probe. Rollback
  renames back.

**Files restore** (`target: site:*`):
- `mode: copy` extracts to `/var/www/<id>/restore-<ts>/`. The UI lists the files.
- Swap renames each restored top-level path over the live one, keeping
  `<path>.old-<ts>`.
- Both are snapshotted at the path level by recording the renames.

**Server config restore** is **not** offered in the UI. It is a manual recovery, and
the documented flow is `nexacloud doctor` plus the recovery kit.

## 7. Disk breakdown and cleanup

`GET /api/disk` (P1) returns cached values (scan TTL 1 h). `POST /api/disk/scan` (J,
heavy, lock `disk-scan`) refreshes them.
```json
{"mount":"/","total":107374182400,"used":40802189312,"scanned_at":"…",
 "parts":[{"id":"projects","label":"Project files","bytes":16106127360},{"id":"backups","label":"Local backups","bytes":10522669875},
          {"id":"system","label":"System & packages","bytes":5583457484},{"id":"logs","label":"Logs","bytes":4939212390},
          {"id":"databases","label":"Databases","bytes":2684354560},{"id":"other","label":"Other","bytes":966367641}],
 "growth":{"bytes_per_week":429496729,"full_at":"2029-10-01","samples":30},
 "cleanup":[{"id":"logs","label":"Rotate & compress logs","bytes":4187593113},{"id":"releases","label":"Prune old releases","bytes":1288490188},
            {"id":"caches","label":"Clear Composer/npm caches","bytes":1181116006},{"id":"offload_backups","label":"Move local backups to S3","bytes":9019431321,"destination_id":3}]}
```
Each part is measured with `du -sxb --apparent-size=no <paths>`, run under
`ionice -c3 nice -n19`:

| Part | Paths |
|---|---|
| `projects` | `/var/www`, minus logs |
| `backups` | `backup_dir` |
| `system` | `/usr`, `/opt`, `/boot`, `/var/lib/apt`, `/var/cache/apt`, `/var/lib/snapd` |
| `logs` | `/var/log`, and `storage/logs` under each site |
| `databases` | `/var/lib/mysql`, `/var/lib/postgresql`, `/var/lib/redis` |
| `other` | `used` minus the sum of the above, floored at 0 |

`growth` is a linear regression over the daily `used` samples recorded by the metrics
sampler (advisor.md §5). It needs at least 7 samples, otherwise it is `null`.

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/disk/cleanup/preview` / `apply` | C, `reversible:false` | ✓* (`offload_backups` deletes local copies) | `logrotate` / `site:<id>` for each site touched / `backup-plan:*` |

Body: `{"actions":["logs","caches"],"site_id":null}`. `site_id` scopes the action to
one site, which is the project detail "Rotate logs" button. The preview lists every path
and the bytes it reclaims.

| Action | Effect |
|---|---|
| `logs` | `logrotate --force /etc/logrotate.d/nexacloud-<slug>` for each site, then gzip `storage/logs/laravel-*.log` older than 2 days and delete those older than `log_keep_days` (14). nginx and PHP logs are rotated with `logrotate --force /etc/logrotate.d/nginx`. Vendor configs are used as-is. |
| `releases` (P2) | Delete `releases/*` beyond `keep` (5), never `current` or the previous release. |
| `caches` | Run as each site user: `composer clear-cache` and `npm cache clean --force`. Also `apt-get clean`. |
| `offload_backups` | For artifacts with a **verified** remote copy on the chosen destination, delete the local copy if it is older than 2 days. Keep the newest local copy per target. |

## 8. Open questions
- **Q13.** The mockup's built-in "Project files" plan backs up to S3 weekly. Should a
  files plan also be created by default (local only) on first start, or only when a
  project exists?
- **Q14.** The rclone version: distro rclone on 22.04 is 1.53, which may lack the
  `Synology` provider. Should we always install the upstream binary?
