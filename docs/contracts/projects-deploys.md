# Projects, deploys and project detail (Phase 2; prober and preflight in Phase 1)

Conventions are in [README.md](README.md). A **project** is a site of kind
`laravel|php|static` plus optional Git/deploy configuration. `project.id` is the site
id. Proxy, redirect and panel sites are never projects.

**Sudo baseline.** Every class C or D `apply` needs sudo (README §1.9, following the
Phase 0 precedent). On preview/apply rows, the `Sudo` column marks whether the
**preview** also needs sudo. On J and S rows, it marks the endpoint itself.

## 1. Model

`projects` table, 1:1 with `sites`:

| Column | Type / validation | Default |
|---|---|---|
| `site_id` | FK | |
| `repo_url` | `^git@github\.com:[A-Za-z0-9_.-]{1,100}/[A-Za-z0-9_.-]{1,100}(\.git)?$` or `^https://github\.com/[A-Za-z0-9_.-]{1,100}/[A-Za-z0-9_.-]{1,100}(\.git)?$` (normalised to the `git@` form). GitHub only in P2 (Q15). | null |
| `repo_full_name` | `owner/repo`, derived | |
| `branch` | `^[A-Za-z0-9_][A-Za-z0-9._/-]{0,99}$`; must not contain `..` or `//`; must not end in `.lock` or `/` | `main` |
| `deploy_mode` | `releases\|in_place` | `releases` for new sites, `in_place` for adopted sites |
| `keep_releases` | 2–20 | 5 |
| `shared_paths` | list of `^[A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)*$`, ≤ 10 | laravel `["storage", ".env"]`; php `[".env"]` plus "Keep uploads/" adds `public/uploads` |
| `trigger` | `{"type":"push","branch":"…"}` \| `{"type":"tag","pattern":"v*"}` (pattern `^[A-Za-z0-9._/*-]{1,50}$`) \| `{"type":"manual"}` | push on `branch` |
| `source` | `webhook\|actions\|poll` | `webhook` if `public_url` is set, else `poll` |
| `poll_interval_s` | 60–3600 | 120 |
| `composer_install`, `npm_build`, `run_tests`, `run_migrations`, `backup_before_migrate` | bool | `true, auto, false, true(laravel), true` |
| `build_hook` | ≤ 16 KiB text | per kind (§5.4) |
| `health_path` | `^/[A-Za-z0-9._~/-]{0,128}$` | laravel `/up` if `routes/web.php` or `bootstrap/app.php` contains `health: '/up'`, else `/` |
| `memory_max` | `^[0-9]{3,4}M$`, 256M–4096M | `1536M` |
| `front_controller`, `web_root` | as in sites.md | |

`npm_build: auto` means run it if `package.json` has a `build` script.

**Deploys table** `deploys`: `id`, `site_id`, `number` (per-site sequence, shown as
"#148"), `trigger` (`manual|webhook|poll|actions|redeploy|rollback|activate`), `ref`,
`sha`, `message`, `author`, `status` (`queued|running|live|failed|cancelled|rolled_back`),
`job_id`, `release`, `previous_release`, `previous_sha`, `backup_artifact_id`, `steps`
(JSON), `note`, `started_at`, `finished_at`.

## 2. Read endpoints

`GET /api/projects` (P2) returns an array:
```json
[{"id":"shop.nexadev.io","kind":"laravel","framework":"Laravel 12","repo":"nexadev/shop","branch":"main","php":"8.3",
  "db":{"engine":"mysql","name":"shop_prod"},"extras":["2 workers"],"status":"healthy|warning|critical","status_text":"Healthy",
  "p95_ms":142,"uptime_30d":99.98,"disk_bytes":3328599654,"last_deploy":{"id":91,"number":212,"status":"live","at":"…"},"managed":true}]
```
- `framework`:
  - Laravel: `laravel/framework` major version from `composer.lock`.
  - Plain PHP: `"PHP (OOP)"` if `composer.json` exists, else `"PHP"`.
  - Static: `"Static"`.
- `db`: from `.env` `DB_CONNECTION`/`DB_DATABASE`, mapping `mysql|mariadb` → `mysql`
  and `pgsql` → `postgres`.
- `extras`: worker count, `"Scheduler"`, `"Redis cache"` (`CACHE_STORE`/`CACHE_DRIVER`
  = redis), `"Composer"`.
- `status`: the worst of the health checks (§9.1) and open advisor findings for the site.
- `disk_bytes`: from the disk scan cache.

`GET /api/projects/{id}` (P2) returns the list item plus `settings` (the model above),
`url`, `paths`, `tags[]` (the ProjectDetail header chips), and `maintenance`
(`{"enabled":false}`).

## 3. New-project wizard

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/projects/preview` | C (composite) | – | – |
| POST | `/api/projects/apply` | C | – | `site:{id}`, `nginx`, `php:<v>`, `db:<engine>/<db>`, `cron`, `supervisor`, `certbot`; heavy (first deploy) |
| PUT | `/api/projects/{id}/settings` | S, or C when `web_root`/`front_controller` change (it delegates to sites options) | – | `site:{id}` |

Body:
```json
{
  "kind": "laravel",
  "domain": "blog.nexadev.io",
  "php_version": "8.4",
  "web_root": "public",
  "deploy_mode": "releases",
  "repo": {"url": "git@github.com:nexadev/blog.git", "branch": "main"},
  "db": {"engine": "mysql", "name": "blog", "user": "blog"},
  "extras": {"ssl": true, "nightly_backup": true, "queue_worker": {"processes": 2}, "scheduler": true, "npm_build": true, "composer_install": true},
  "github": {"bootstrap_with_pat": true},
  "clone_from": null
}
```
- `db` may be `null` ("None"). `db.name` and `db.user` default to the slug.
- `queue_worker` (1–8 processes), `scheduler` and `db` are laravel-only.
- `clone_from` is `{"site_id","branch"}` for the Domains "Project (branch: develop)"
  subdomain. It copies `repo`, `kind`, `php_version` and `db` engine. It never copies
  DB data.

Generated values (`APP_KEY`, DB password, webhook secret, deploy key) follow README
rule R1: the preview shows placeholders, and the job result returns them once.

**Plan steps**, in the preview's `steps`, which the UI renders as "What will happen":
1. Site create (sites.md §4.2): user, dirs, pool, vhost.
2. DB + user (database-manager.md §6). The password is generated at apply.
3. `shared/.env`:
   - Copied from the repo's `.env.example` after the first fetch, else from a built-in
     Laravel template.
   - Set: `APP_NAME`, `APP_ENV=production`, `APP_DEBUG=false`,
     `APP_URL=https://<domain>`, `APP_KEY=base64:<32 random bytes>`, `DB_*`.
   - When Redis is installed: `QUEUE_CONNECTION=redis` and `CACHE_STORE=redis`.
4. The deploy key (§4).
5. Optional PAT bootstrap (§4.2).
6. Worker programs (§9.3) and the scheduler cron (§9.5).
7. The SSL issuance follow-up.
8. Adds the DB to the "All databases" plan (backups.md). Nothing to do when it covers
   `*`.
9. The first deploy (§5).

**Apply job.**
- With a PAT, or when the repo is public over HTTPS: runs straight through to the
  first deploy.
- Otherwise it stops after step 7 with `result.state = "awaiting_github"`:
  ```json
  {"state":"awaiting_github","public_key":"ssh-ed25519 AAAA… nexacloud@nexadev.io:blog","webhook":{"url":"https://panel.nexadev.io/hooks/github/blog.nexadev.io","secret":"<shown once>","content_type":"application/json","events":["push"]}}
  ```
  The UI then calls `POST /api/projects/{id}/github/verify`, and on success
  `POST /api/projects/{id}/deploys`.

## 4. Deploy keys, webhook secret and GitHub

### 4.1 Deploy key
- Generated with
  `ssh-keygen -t ed25519 -N "" -C "nexacloud@<hostname>:<slug>" -f /var/lib/nexacloud/keys/<site_id>/id_ed25519`.
- The directory is mode 0700 and the private key 0600, both owned by the site user.
  Git runs as that user.
- GitHub host keys are pinned in `/etc/nexacloud/github_known_hosts` (0644). This file
  is written from keys embedded in the binary; there is no TOFU.
- Every git call uses this env:
  `GIT_SSH_COMMAND="ssh -i /var/lib/nexacloud/keys/<id>/id_ed25519 -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/etc/nexacloud/github_known_hosts -o BatchMode=yes"`
  plus `GIT_TERMINAL_PROMPT=0`.

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| GET | `/api/projects/{id}/deploy-key` | – | – | – |
| POST | `/api/projects/{id}/deploy-key/rotate/preview` / `apply` | C | – | `site:{id}` |
| POST | `/api/projects/{id}/github/verify` | J | – | `site:{id}` |
| POST | `/api/projects/{id}/webhook-secret/rotate` | S (returns the new secret once) | ✓ | – |

- GET deploy-key returns:
  `{"public_key","fingerprint":"SHA256:…","created_at","verified_at","webhook":{"url","last_delivery_at","last_ping_at"}}`.
- Verify runs `git ls-remote --heads <repo_url>` as the site user. Result:
  `{"ok":true,"branches":["main","develop"],"default_branch":"main"}`. Failure gives
  `ok:false` with `hint:"Add the deploy key in GitHub → Settings → Deploy keys (read-only)"`.
- The webhook secret is 32 random bytes as hex, stored in
  `/etc/nexacloud/secrets/github/<site_id>.webhook` (0600).

### 4.2 Optional PAT (bootstrap and repo picker)
| Method | Path | Class | Sudo |
|---|---|---|---|
| PUT | `/api/github/pat` | S | ✓ |
| DELETE | `/api/github/pat` | S | ✓ |
| GET | `/api/github/status` | – | – |
| GET | `/api/github/repos` | – | – |
| GET | `/api/github/repos/{owner}/{repo}/branches` | – | – |
| POST | `/api/projects/{id}/github/bootstrap` | J | – |

- **Storing a PAT.** Body `{"token":"github_pat_…"}`. The token must match
  `^(ghp_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{22,255})$`. It is validated with
  `GET https://api.github.com/user`, then stored in `/etc/nexacloud/secrets/github/pat`.
  The token goes in the `Authorization` header and is never logged.
- **Fine-grained permissions needed:** Metadata R, Contents R, Administration RW (deploy
  keys), Webhooks RW, Secrets RW (only for `source: actions`).
- **Status.** `GET /api/github/status` returns
  `{"pat":true,"login":"nexadev","repo_count":7,"missing_permissions":[]}`. This is the
  Deployments header.
- **Repo picker.** `GET /api/github/repos` calls
  `GET /user/repos?per_page=100&sort=pushed`, paginated up to 500 and cached 5 min. Each
  item is `{"full_name","private","default_branch","ssh_url"}`.
- **Bootstrap** makes these idempotent GitHub calls:
  - `POST /repos/{o}/{r}/keys`, body `{"title":"NexaCloud <hostname>","key":"<pub>","read_only":true}`.
    Skipped if the fingerprint is already listed.
  - `POST /repos/{o}/{r}/hooks`, body
    `{"name":"web","active":true,"events":["push"],"config":{"url":"<public_url>/hooks/github/<id>","content_type":"json","secret":"<secret>","insecure_ssl":"0"}}`.
    Skipped without `public_url`.
  - Only for `source: actions`:
    1. Fetch the repo key with `GET /repos/{o}/{r}/actions/secrets/public-key`.
    2. Set each secret with `PUT /repos/{o}/{r}/actions/secrets/{NAME}`, sending a
       libsodium sealed box (crate `crypto_box`, `seal` feature).
    3. The secrets are `NEXACLOUD_DEPLOY_HOOK` and `NEXACLOUD_WEBHOOK_SECRET`.

  The result is `{"deploy_key":"added|exists","webhook":"added|exists|skipped_no_public_url","secrets":["…"]}`.

## 5. Deploy pipeline

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/projects/{id}/deploys` | J, heavy | – | `site:{id}`, `php:<v>`, + `db:<engine>/<db>` if backup or migrate may run |
| POST | `/api/deploys/{id}/redeploy` | J, heavy | – | same |
| POST | `/api/deploys/{id}/rollback` | J | – | `site:{id}`, `php:<v>` |
| POST | `/api/projects/{id}/releases/{release}/activate` | J | – | `site:{id}`, `php:<v>` |
| POST | `/api/jobs/{job_id}/cancel` (P0) | – | ✓ (Phase 0) | – | Allowed before `switch`. Runs `systemctl stop nc-deploy-…` and removes the release dir. |

**Deploy body:** `{"ref":null,"sha":null,"force":false}`.
- `ref` defaults to the trigger branch.
- `sha` must match `^[0-9a-f]{7,40}$` and pins a commit.
- `force` matters only in in-place mode: it allows `reset --hard` over local changes or
  non-fast-forward history.

The response is `202 {"deploy_id":92,"number":213,"job_id":"…"}`.

**Coalescing.** If a deploy for the site is still `queued` (not started), a new trigger
replaces its `sha` and returns the same `deploy_id`. Manual deploys never coalesce.

### 5.1 Step runner (systemd-run)
Every step that runs project code runs as the site user. The argv, with the variable
parts in angle brackets:
```
systemd-run --quiet --wait --pipe --collect
  --unit=nc-deploy-<slug>-<number>-<step>
  --uid=<user> --gid=<group>
  --working-directory=<dir>
  --property=MemoryMax=<memory_max> --property=MemorySwapMax=0
  --property=CPUQuota=150% --property=TasksMax=512
  --property=RuntimeMaxSec=<step timeout>
  --property=Nice=10 --property=IOSchedulingClass=best-effort --property=IOSchedulingPriority=7
  --property=NoNewPrivileges=yes --property=PrivateTmp=yes
  --setenv=HOME=/var/lib/nexacloud/home/<user> --setenv=COMPOSER_HOME=/var/lib/nexacloud/home/<user>/.composer
  --setenv=npm_config_cache=/var/lib/nexacloud/home/<user>/.npm --setenv=PATH=/usr/local/bin:/usr/bin:/bin
  --setenv=GIT_SSH_COMMAND=<…> --setenv=GIT_TERMINAL_PROMPT=0
  --setenv=RELEASE=<release dir> --setenv=SHARED=<base>/shared --setenv=APP_ROOT=<app_root>
  --setenv=PHP=/usr/bin/php<v> --setenv=NEXACLOUD_DEPLOY=<number>
  -- /bin/bash -euo pipefail -c '<step script>'
```
- `<step script>` is a fixed string from the table below. Only `build_hook` is
  operator-authored.
- stdout and stderr stream into the job log.
- The exit code decides the step result.
- Root-only steps (`reload`, writing symlinks for nginx) run in the agent.

**Step timeouts:**

| Step | Timeout |
|---|---|
| fetch | 300 s |
| composer | 900 s |
| npm | 1200 s |
| build_hook | 900 s |
| tests | 1200 s |
| migrate | 600 s |
| others | 60 s |

### 5.2 Releases mode (zero-downtime)
`<base>/repo.git` is a bare mirror. Releases live in `<base>/releases/<YYYYmmddHHMMSS>`,
alongside `<base>/shared/` and `<base>/current -> releases/<ts>`.

| key | UI name | Script / action | Skipped when |
|---|---|---|---|
| `fetch` | Fetch code | First time: `git clone --mirror <url> <base>/repo.git`. Otherwise: `git -C <base>/repo.git remote update --prune`. Then `sha=$(git -C repo.git rev-parse <ref>^{commit})`, `mkdir releases/<ts>`, `git -C repo.git archive <sha> \| tar -x -C releases/<ts>`, and write `releases/<ts>/.nexacloud-release` (`{sha,ref,number,message,author}`). | – |
| `link_shared` | (part of fetch) | For each shared path: `rm -rf "$RELEASE/<p>"; ln -s "$SHARED/<p>" "$RELEASE/<p>"`. The first deploy seeds `shared/<p>` from the release if it is missing. | – |
| `composer` | Install PHP packages | `$PHP /usr/local/bin/composer install --no-dev --prefer-dist --no-interaction --no-progress --optimize-autoloader` | no composer.json or `composer_install:false` |
| `npm` | Build front-end | `npm ci --no-audit --no-fund && npm run build && rm -rf node_modules` | rule above |
| `build_hook` | Custom build | the operator's script | empty |
| `tests` | Run tests | §5.5 | `run_tests:false` |
| `db_backup` | Back up database | Run `$PHP artisan migrate --pretend --force` first. If its output contains SQL, call the backup runner (backups.md §4.1, `pre_deploy`). | not laravel, no DB, `backup_before_migrate:false`, or nothing pending |
| `migrate` | Migrate database | `$PHP artisan migrate --force` | not laravel or `run_migrations:false` |
| `optimize` | Cache config & routes | `$PHP artisan optimize` | not laravel |
| `switch` | Switch release | `ln -s releases/<ts> <base>/current.tmp-<ts> && mv -T <base>/current.tmp-<ts> <base>/current` (atomic rename, as the site user) | – |
| `reload` | Reload | agent: `systemctl reload php<v>-fpm`. FrankenPHP: frankenphp.md §7. | static |
| `queue_restart` | (part of reload) | `$PHP artisan queue:restart` | no workers |
| `probe` | Health check | agent: `GET https://<primary><health_path>`, resolved to 127.0.0.1 with SNI. Pass = 200–399. 3 attempts, 2 s apart, 10 s timeout each. | – |
| `prune` | (after success) | Delete the oldest `releases/*` beyond `keep_releases`. Never `current` or `previous_release`. | – |

**On failure before `switch`.** The deploy is `failed`. The release dir is deleted and
live is untouched. The note reads: "Nothing changed on the live site. The previous
release (#147) is still serving traffic."

**On failure at `probe`:**
1. Auto-rollback: switch `current` back to `previous_release`, reload, and probe again.
2. The status becomes `rolled_back`.
3. Migrations are **not** reverted. The note links the `db_backup` restore point when
   one exists.

### 5.3 In-place mode (adopted sites)
1. **fetch.** `git -C <base> fetch --prune origin`. Then check for local changes:
   `git -C <base> status --porcelain --untracked-files=no`. Non-empty output fails
   without `force`.
2. Record `previous_sha` from `git rev-parse HEAD`.
3. Move to the new commit with `git merge --ff-only <sha>`. If it cannot fast-forward,
   fail without `force`. With `force`, run `git reset --hard <sha>`.
4. composer → npm → build_hook → tests → db_backup → migrate → optimize, the same
   scripts in `<base>`. The optional `maintenance_during_migrate` wraps migrate in
   `artisan down` / `up`.
5. reload → queue_restart → probe.
6. **Rollback** (auto or manual):
   1. `git reset --hard <previous_sha>`
   2. composer install
   3. optimize
   4. reload
   5. probe

The UI shows "Not zero-downtime: files change in place".

### 5.4 Build hook and settings
| Method | Path | Class | Sudo |
|---|---|---|---|
| GET | `/api/projects/{id}/deploy-settings` | – | – |
| PUT | `/api/projects/{id}/deploy-settings` | S | ✓ if `build_hook` changes |
| GET | `/api/projects/{id}/deploy-settings/defaults` | – | – |

- **GET deploy-settings** returns:
  - the model fields from §1
  - `script_preview`: read-only text showing the full pipeline as a shell-like script.
    This is the mockup's "Deploy script" box. Only the `# --- custom build hook ---`
    section is editable.
  - `webhook_url`
- **Default `build_hook`:**
  - laravel: `$PHP artisan storage:link --force`
  - php and static: empty
- **"Reset to Laravel default"** = `PUT` with the value from `/defaults`.
- **Mockup toggles:**
  - "Zero-downtime releases" = `deploy_mode`
  - "Stop if tests fail" = `run_tests`
  - "Back up database before migrate" = `backup_before_migrate`
  - "Notify on failure" is OUT (README Q8)

### 5.5 Tests on the server (`run_tests`)
- The command is `$PHP artisan test --parallel` when `brianium/paratest` is in
  `composer.lock`, otherwise `$PHP artisan test`.
- It runs **before** `link_shared` with:
  `APP_ENV=testing DB_CONNECTION=sqlite DB_DATABASE=:memory: CACHE_STORE=array QUEUE_CONNECTION=sync SESSION_DRIVER=array MAIL_MAILER=array`.
- If `.env.testing` exists in the repo, it is used instead.
- **Refusal.** The step fails (it does not run) if `phpunit.xml` sets `DB_DATABASE` to
  the production database name. Running tests against production is never allowed.
- The UI recommends Actions for tests.

### 5.6 Runs and releases
`GET /api/deploys?site_id=&limit=&cursor=` returns paginated items:
`{"id","site_id","number","sha","message","author","branch","status","trigger","started_at","finished_at","duration_ms"}`.

`GET /api/deploys/{id}` returns the item plus:
```json
{"steps":[{"key":"composer","name":"Install PHP packages","cmd":"composer install --no-dev -o","status":"ok|failed|skipped|running|pending","result":"Done","duration_ms":18000}],
 "log_tail":"…last 64 KiB…","note":"Zero-downtime: the new release only went live after the health check passed.","job_id":"…","release":"releases/20261112160102","backup_point_id":812}
```
`result` strings are short: `Done`, `Passed`, `1 failed` (parsed from the PHPUnit
summary), `200 OK`, `Skipped`.

`GET /api/projects/{id}/releases` returns
`[{"dir":"releases/20261112160102","number":147,"sha":"3d11f0b","current":true,"previous":false,"bytes":…,"created_at":"…"}]`.
- **Activate** runs `switch`, `reload`, `queue_restart` and `probe`, and records a
  `deploys` row with `trigger: activate`.
- **Warning** returned in the job result: `"Database migrations are not reverted"`.

## 6. Webhook `POST /hooks/github/{site}` (public)
- **Location.** It is outside `/api`, with no cookie and no Origin check, and must be
  routed before the SPA fallback. `{site}` is a site id.
- **Body size.** Max 5 MiB, else `413`.
- **Required header.** `X-Hub-Signature-256: sha256=<hex>`, an HMAC-SHA256 of the
  **raw** body with the site's webhook secret. It is compared in constant time.
- **Unknown site or bad signature.** Both get the same `401 {"error":"unauthorized"}`,
  so site ids cannot be enumerated.
- **Rate limits:**
  - 60 requests/min per IP
  - 30/min per site
  - more than 20 signature failures from one IP in 10 min blocks that IP for 10 min
    (`429`)
- **Dedupe.** Keys are `X-GitHub-Delivery`, or `run_id` for Actions calls. They are kept
  7 days in `webhook_deliveries`. A repeat returns `200 {"ok":true,"duplicate":true}`.

| Event header | Handling |
|---|---|
| `X-GitHub-Event: ping` | `200 {"ok":true,"pong":true}`. Records `last_ping_at`. |
| `X-GitHub-Event: push` | Ignored with `202 {"ok":true,"ignored":"<reason>"}` when `deleted:true`, the ref does not match `trigger` (exact branch `refs/heads/<b>` or tag glob on `refs/tags/<t>`), `source != webhook`, or `trigger.type == manual`. Otherwise it enqueues a deploy of `after` with `message = head_commit.message`, `author = pusher.name`, and returns `202 {"ok":true,"deploy_id":…}`. |
| `X-NexaCloud-Event: deploy` (Actions) | Body `{"ref","after","source":"actions","run_id"}`. Accepted only when `source == actions`. The ref must match the trigger. Enqueue as above. |
| anything else | `202 {"ok":true,"ignored":"event"}` |

Every accepted delivery is audited as actor `github`, with the IP and delivery id.

**Polling fallback** (`source: poll`):
- Every `poll_interval_s`, run `git ls-remote <repo_url> refs/heads/<branch>` (or
  `'refs/tags/<pattern>'`, taking the highest version-sorted tag) as the site user,
  through a short `systemd-run`.
- If the sha differs from both the last deployed sha and the last failed sha, enqueue a
  deploy with `trigger: poll`.
- Errors back off: 2×, up to 30 min.

## 7. Generated GitHub Actions workflow
`GET /api/projects/{id}/actions-workflow` (P2) returns
`{"path":".github/workflows/nexacloud-deploy.yml","content":"…","secrets":[{"name":"NEXACLOUD_DEPLOY_HOOK","value":"https://panel.example.com/hooks/github/<id>"},{"name":"NEXACLOUD_WEBHOOK_SECRET","value":null}]}`.

The secret value is never returned. It is shown once at creation or rotation, or set by
bootstrap. `content` for a laravel project with a push trigger on `production`, PHP 8.2:
```yaml
name: Deploy
on:
  push:
    branches: [production]
jobs:
  test-and-deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: shivammathur/setup-php@v2
        with: { php-version: "8.2" }
      - run: composer install --no-interaction --prefer-dist --no-progress
      - run: cp .env.example .env && php artisan key:generate
      - run: php artisan test
      - name: Trigger NexaCloud deploy
        env:
          NEXA_HOOK: ${{ secrets.NEXACLOUD_DEPLOY_HOOK }}
          NEXA_SECRET: ${{ secrets.NEXACLOUD_WEBHOOK_SECRET }}
        run: |
          body=$(printf '{"ref":"%s","after":"%s","source":"actions","run_id":"%s"}' "$GITHUB_REF" "$GITHUB_SHA" "$GITHUB_RUN_ID")
          sig=$(printf '%s' "$body" | openssl dgst -sha256 -hmac "$NEXA_SECRET" | sed 's/^.* //')
          curl -fsS -X POST "$NEXA_HOOK" -H "Content-Type: application/json" \
               -H "X-NexaCloud-Event: deploy" -H "X-Hub-Signature-256: sha256=$sig" --data "$body"
```
Variations:
- tag trigger: `on: push: tags: ["v*"]`
- plain PHP: no artisan lines; tests are `vendor/bin/phpunit` if present
- static: no PHP steps

"Build on: GitHub Actions" sets `source: actions`. Tests run on GitHub, and the server
still runs the composer and npm build steps. Shipping prebuilt artifacts is out of
scope.

## 8. Install and import wizard

`GET /api/setup/preflight` (**P1**):
```json
{"checks":[{"k":"os","label":"Operating system","value":"Ubuntu 24.04","level":"ok"},
 {"k":"resources","label":"CPU / memory","value":"2 vCPU · 8 GB","level":"ok"},
 {"k":"disk","label":"Free disk","value":"62 GB","level":"ok|warn (<10 GB)|bad (<3 GB)"},
 {"k":"nginx","label":"Existing Nginx","value":"Found · will adopt","level":"info"},
 {"k":"other_panel","label":"Other panel (cPanel, Plesk…)","value":"None","level":"ok"},
 {"k":"port","label":"Port 8443","value":"In use by NexaCloud","level":"ok"}]}
```
- Other panels are detected by these paths: `/usr/local/cpanel`, `/usr/local/psa`,
  `/usr/local/directadmin`, `/usr/local/hestia`, `/usr/local/vesta`, `/usr/local/CyberCP`,
  `/home/clp`.
- Step 2 (panel domain, email, firewall) and step 3 (scan/import) are in sites.md §3 and
  §11, php-services.md §8, and README §3.10.

`GET /api/import/github-matches` (P2):
```json
[{"site_id":"shop.nexadev.io","repo":"nexadev/shop","branch":"main","state":"matched|not_linked|no_git|no_access"}]
```
- `repo` comes from `git -C <dir> remote get-url origin`, parsed.
- `no_access` is set when the PAT cannot see the repo.
- Linking runs `PUT /api/projects/{id}/settings {repo_url, branch}`, then
  `github/verify` and `github/bootstrap`.

## 9. Project detail

### 9.1 Health prober and response times (**P1**)
- **Prober.** Every 60 s for each content or proxy site:
  `GET <scheme>://<primary><health_path>`, resolved to `127.0.0.1` with SNI, 10 s
  timeout, no redirects followed. The site is up when the status is below 500 and the
  connection succeeds.
- **Probe storage.** `probes_1m` (48 h: status, ms) and `probes_5m` (30 d: total, up,
  p50, p95).
- **Access-log timings.** A tailer reads `/var/log/nginx/<id>.access.log` in the
  `nexacloud` format (sites.md §7.2), parsing `$request_time` and `$status`. It writes
  `rt_1m(site, t, count, p50, p95, s5xx)` (48 h) and `rt_5m` (30 d). Unmanaged sites
  without that format fall back to probe data.

`GET /api/sites/{id}/response-times?range=1h|24h|7d` returns
`{"source":"access_log|probe","step_s":300,"points":[{"t","p50","p95","count"}]}`.

`GET /api/sites/{id}/health` returns
`{"status":"healthy|warning|critical","checks":[{"id","name","value","state_text","level","action":null|{…}}]}`:

| id | Name (UI) | Value / rule | Phase |
|---|---|---|---|
| `http` | HTTP check | last probe status. bad when down in 3 consecutive probes | P1 |
| `ssl` | SSL certificate | days left. warn < 14 | P1 |
| `error_rate` | Error rate · 1 h | 5xx / total from access log. warn > 1 %, bad > 5 % | P1 |
| `php_version` | PHP version | warn when ends soon, bad when EOL | P1 |
| `logs_size` | storage/logs | bytes. warn > 1 GB. action = cleanup `logs` scoped to the site | P1 |
| `uploads_size` | uploads/ | bytes (shared `public/uploads` if present) | P1 |
| `queue_pending` | Queue pending | Σ pending (§9.3). warn > 500 | P2 |
| `scheduler` | Scheduler | last tick age. bad > 3 min | P2 |
| `composer` | Composer packages | "3 outdated · 1 security fix", from the last `composer-audit` | P2 |

`POST /api/projects/{id}/composer-audit` (J, P2, lock `site:{id}`) runs, as the site
user, `composer outdated --direct --format=json --no-interaction` and
`composer audit --format=json --no-interaction`. The result is cached.

### 9.2 Artisan runner
`POST /api/projects/{id}/artisan` (J, P2, lock `site:{id}`). Body:
`{"command":"migrate","args":["--step"],"confirm":null}` or `{"line":"cache:clear"}`.
- `line` is free input from the "php artisan" box. It is split on whitespace (no quote
  handling) into a command and args.
- The command runs through §5.1 `systemd-run` in `app_root` with `MemoryMax=512M` and
  `RuntimeMaxSec=300`:
  `$PHP artisan <command> <args…> --no-interaction --no-ansi`.
- The result is `{"exit_code":0,"output":"…≤64 KiB…"}`.

| Command | Allowed args (regex per arg) | Confirm (typed site id) | Sudo |
|---|---|---|---|
| `migrate` | `--step`, `--pretend` (`--force` added automatically) | ✓ when `APP_ENV=production` | – |
| `migrate:status`, `about`, `schedule:list`, `queue:failed`, `route:list`, `horizon:status` | – | – | – |
| `optimize`, `optimize:clear`, `cache:clear`, `config:cache`, `config:clear`, `route:cache`, `route:clear`, `view:cache`, `view:clear`, `event:cache`, `event:clear`, `queue:restart`, `storage:link`, `horizon:terminate` | – | – | – |
| `queue:retry` | `^(all|[0-9a-f-]{36})$` (1–20 args) | – | – |
| `queue:flush`, `queue:clear` | `--queue=[A-Za-z0-9_,-]{1,64}` | ✓ | – |
| `migrate:rollback` | `--step=[1-9][0-9]?` | ✓ | ✓ |
| `db:seed` | `--class=[A-Za-z0-9\\]{1,128}` | ✓ | ✓ |
| `down`, `up` | refused. Use §9.8. | | |
| anything else (incl. `tinker`, `migrate:fresh`, `migrate:reset`, `db:wipe`, `key:generate`) | refused with `400 invalid` | | |

The mockup buttons are migrate, optimize, cache:clear, config:cache, queue:restart and
storage:link.

### 9.3 Queue workers (Supervisor)
**File** `/etc/supervisor/conf.d/nc-<slug>-<name>.conf`:
```ini
; Managed by NexaCloud
[program:nc-shop-default]
process_name=%(program_name)s_%(process_num)02d
command=/usr/bin/php8.3 /var/www/shop.example.com/current/artisan queue:work redis --queue=default --sleep=3 --tries=3 --timeout=90 --max-time=3600 --memory=256
directory=/var/www/shop.example.com/current
user=nc_shop
numprocs=2
autostart=true
autorestart=true
startsecs=1
stopasgroup=true
killasgroup=true
stopwaitsecs=100
redirect_stderr=true
stdout_logfile=/var/log/nexacloud/workers/nc-shop-default.log
stdout_logfile_maxbytes=10MB
stdout_logfile_backups=3
```
`stopwaitsecs` is `timeout` + 10.

**Worker fields:**

| Field | Validation |
|---|---|
| `name` | `^[a-z0-9_-]{1,24}$` |
| `connection` | `^[a-z0-9_-]{1,32}$`, default from `.env` `QUEUE_CONNECTION` |
| `queue` | `^[A-Za-z0-9_,:-]{1,100}$` |
| `processes` | 1–8 |
| `tries` | 1–10 |
| `timeout` | 10–3600 |
| `memory` | 64–1024 |
| `sleep` | 0–60 |
| `max_time` | 0–86400 |

Imported external programs are listed with `external:true` and are read-only, except
restart.

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| GET | `/api/projects/{id}/workers` | – | – | – |
| POST | `/api/projects/{id}/workers/preview` / `apply` | C (restart tier) | – | `site:{id}`, `supervisor` |
| POST | `/api/projects/{id}/workers/restart` | J | – | `supervisor` |
| POST | `/api/projects/{id}/workers/{name}/restart` | J | – | `supervisor` |

- **Preview/apply body:** `{"upsert":[{…worker…}],"delete":["exports"]}`. Apply runs
  `supervisorctl reread`, then `supervisorctl update nc-<slug>-<name>`.
- **Restart** runs `supervisorctl restart nc-<slug>-<name>:*`.

GET returns rows like:
```json
[{"name":"crm-default","program":"nc-crm-default","queue":"default","processes":2,"pending":38,"failed_24h":0,"status":"running|stalled|fatal|stopped|starting","uptime_s":533000,"external":false}]
```
- `status`: from `supervisorctl status nc-<slug>-<name>:*`.
- `pending`, best-effort, with the source given in `pending_source`:
  - redis: `LLEN` + `ZCARD :delayed` + `ZCARD :reserved` on `<prefix>queues:<q>`.
    `prefix` = `.env` `REDIS_PREFIX`, else `Str::slug(APP_NAME,'_').'_database_'`.
    DB from `REDIS_DB`, default 0.
  - database: `SELECT COUNT(*) FROM jobs WHERE queue = ?`, via `nexacloud_admin`.
- `failed_24h`: `SELECT COUNT(*) FROM failed_jobs WHERE failed_at >= NOW() - INTERVAL 1 DAY`.
- **Stall rule:** see §9.4.

### 9.4 Stall detection
A worker's status (also advisor rule `supervisor-stalled`) is `stalled` when the
program is RUNNING, `pending > 0` for ≥ 10 min, and no `DONE`/`FAIL` line has been
appended to the worker log in that window. Laravel ≥ 9 prints one per job.

### 9.5 Scheduler
**File** `/etc/cron.d/nexacloud-<slug>` (mode 0644; the name contains no dots):
```
# Managed by NexaCloud
SHELL=/bin/sh
PATH=/usr/local/bin:/usr/bin:/bin
* * * * * nc_shop cd /var/www/shop.example.com/current && /usr/bin/php8.3 artisan schedule:run >> /var/log/nexacloud/scheduler/shop.log 2>&1
```
| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| GET | `/api/projects/{id}/scheduler` | – | – | – |
| POST | `/api/projects/{id}/scheduler/preview` / `apply` | C | – | `site:{id}`, `cron` |

The body is `{"enabled":true}`. GET returns:
```json
{"enabled":true,"last_tick_at":"…","entries":[{"command":"reports:daily","expression":"0 6 * * *","human":"Daily 06:00","next_due":"…","last_run":"…","last_result":"ok|failed|running|unknown"}]}
```
- `entries` come from `php artisan schedule:list` (text parsed; cached 5 min).
- `last_run` and `last_result` come from the scheduler log lines
  `YYYY-MM-DD HH:MM:SS Running [...] … DONE|FAIL`. This is best-effort.
- `last_tick_at` is the log mtime.

### 9.6 Logs
| Method | Path | Notes |
|---|---|---|
| GET | `/api/sites/{id}/logs` | `[{"id":"laravel","label":"laravel.log","path":"…","bytes","mtime"}, {"id":"nginx-access"}, {"id":"nginx-error"}, {"id":"php-fpm","label":"php8.2-fpm.log"}, {"id":"worker:crm-default"}, {"id":"scheduler"}]` |
| GET | `/api/sites/{id}/logs/{log}?lines=200&grep=` | `lines` 1–2000. `grep` is a literal substring, ≤ 100 chars. Returns `{"lines":[…],"truncated":bool}`. |
| GET | `/api/sites/{id}/logs/{log}/stream` | SSE `log` events (README §1.11). Follows rotation (re-opens on inode change). Starts with the last 50 lines. |
| GET | `/api/sites/{id}/logs/{log}/download` | Streams the file, `Content-Disposition: attachment`. |

- `{log}` must match `^[a-z0-9:_-]{1,64}$` **and** be in the list. Clients never pass
  paths.
- For Laravel's `daily` channel, `laravel` resolves to the newest `laravel-*.log`.

`GET /api/sites/{id}/errors?since=1h|24h|7d` (P2) returns
`[{"level":"ERROR","last_at":"…","count":312,"message":"Symfony\\…TransportException: Connection to smtp.mailgun.org:587 timed out","file":"storage/logs/laravel.log"}]`.
- Laravel log entries are parsed from `[YYYY-MM-DD HH:MM:SS] env.LEVEL: message` (first
  line only). PHP log lines are parsed from `PHP <Level>: … in <file> on line <n>`.
- Entries are grouped by level plus the message with digits replaced by `N`.
- The top 20 by `last_at` are returned. The parser reads at most the last 50 MB.

### 9.7 .env editor
| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| GET | `/api/projects/{id}/env` | – | – | – |
| POST | `/api/projects/{id}/env/reveal` | – (audited) | ✓ | – |
| POST | `/api/projects/{id}/env/preview` / `apply` | C (restart: `php<v>-fpm` reload) | ✓* if `raw` | `site:{id}`, `php:<v>` |

- **Location.** In releases mode the file is `shared/.env`, else `<base>/.env`.
- **GET** returns
  `{"path","sha256","entries":[{"key":"APP_KEY","display":"••••••••••••••••••••","masked":true,"line":3}],"raw_available":true}`.
  A value is masked when either:
  - the key matches `(?i)(KEY|SECRET|PASSWORD|PASS|TOKEN|PRIVATE|CREDENTIAL|DSN|AUTH)`, or
  - the value contains `://user:pass@`.

  Unmasked values are returned as `value`.
- **Reveal** body: `{"keys":["APP_KEY"]}`. It returns `{"values":{"APP_KEY":"base64:…"}}`.
- **Edit** body: `{"set":{"APP_DEBUG":"false"},"unset":["OLD_KEY"]}` or
  `{"raw":"…full file…"}`. Raw requires sudo.
  - Key: `^[A-Z_][A-Z0-9_]{0,63}$`.
  - Value: ≤ 4096 bytes, no CR, LF or NUL.
  - Values matching `^[A-Za-z0-9_./:@,+-]*$` are written bare. Others are
    double-quoted with `\`, `"` and `$` backslash-escaped.
  - Order and comments are preserved. New keys are appended.
  - The diff is `secret:true`.
  - The file is written as `<user>:<group>` 0640.
- **Post-write steps** ("Saving runs config:cache and reloads PHP-FPM"):
  1. `$PHP artisan config:cache`, if `bootstrap/cache/config.php` exists.
  2. `systemctl reload php<v>-fpm`.
  3. `$PHP artisan queue:restart`, if the site has workers.
- **Phase.** The planner is P1 (advisor `app-debug-production`). The UI is P2.

### 9.8 Maintenance mode
`POST /api/sites/{id}/maintenance/preview` and `/apply` (C, safe, lock `site:{id}`).
Body `{"enabled":true,"bypass_secret":true,"retry":60}`.
- **laravel.** `$PHP artisan down --retry=<retry> [--secret=<random 24 chars>]` or
  `artisan up`, via §5.1. The result includes `bypass_url`, shown once.
- **Other kinds.** Create or remove `/var/lib/nexacloud/maintenance/<id>`, then
  `/usr/share/nexacloud/__nexacloud_maintenance.html` is served as 503 (sites.md §7.2).
  No reload is needed.

Restore-replace (backups.md §6.3) uses the same mechanism.

## 10. Open questions
- **Q15.** Non-GitHub remotes (GitLab, Bitbucket, self-hosted) need known_hosts TOFU and
  different webhook signatures. Defer until after P2?
- **Q16.** Should in-place sites be offered a one-click "convert to releases mode"? It
  moves files and changes the nginx root, so it needs a careful plan.
- **Q17.** Webhooks need `public_url` (the panel domain) for GitHub to deliver over valid
  TLS. Without it, the default is polling. Should we also offer GitHub's "disable SSL
  verification" option against `:8443`? Not recommended.
