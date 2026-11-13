#!/usr/bin/env bash
# End-to-end test: build a Linux package, boot an Ubuntu 24.04 "VPS" container
# with systemd + nginx + PHP + MySQL + PostgreSQL, run the real installer and
# exercise every API endpoint, then upgrade, reset password and uninstall.
#
#   bash tests/docker/run-e2e.sh            # full run, removes the container at the end
#   KEEP=1 bash tests/docker/run-e2e.sh     # leave the container running (panel on https://127.0.0.1:18443)
#   SKIP_BUILD=1 ...                        # reuse dist/ from a previous run
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
NAME=nexacloud-e2e
IMAGE=nexacloud-e2e:24.04
PORT=18443
U="https://127.0.0.1:$PORT"
PASSES=0

step() { printf '\n\033[1;36m▶ %s\033[0m\n' "$*"; }
pass() { PASSES=$((PASSES+1)); printf '  \033[32m✓\033[0m %s\n' "$*"; }
fail() { printf '  \033[31m✗ %s\033[0m\n' "$*" >&2; exit 1; }
check() { local desc="$1"; shift; if "$@" >/dev/null 2>&1; then pass "$desc"; else fail "$desc"; fi; }
in_box() { docker exec -i "$NAME" bash -c "$*"; }
api() { curl -fsSk -b "$JAR" "$U/api/$1"; }
jqe() { printf '%s' "$1" | jq -e "$2" >/dev/null; }
code() { curl -sk -o /dev/null -w '%{http_code}' "$@"; }

cleanup() {
  if [ "${KEEP:-0}" != "1" ]; then docker rm -f "$NAME" >/dev/null 2>&1 || true; fi
  rm -f "${JAR:-}"
}
trap cleanup EXIT

# ------------------------------------------------------------------ build
if [ "${SKIP_BUILD:-0}" != "1" ]; then
  step "Build web UI"
  (cd web && npm ci --silent && npx nuxi generate >/dev/null)
  pass "web/.output/public built"

  step "Build Linux agent (in Docker, Debian bullseye glibc for wide compatibility)"
  docker volume create nexacloud-cargo >/dev/null
  docker run --rm --platform linux/amd64 \
    -v "$root/agent:/src" -v nexacloud-cargo:/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR=/src/target/linux -w /src \
    rust:1-bullseye cargo build --release --locked
  pass "agent built"

  step "Package"
  NEXACLOUD_BIN="$root/agent/target/linux/release/nexacloud" bash scripts/package.sh >/dev/null
  pass "dist/nexacloud-x86_64-linux.tar.gz"
fi

# ------------------------------------------------------------------ boot "VPS"
step "Boot Ubuntu 24.04 container with systemd"
docker build -q -t "$IMAGE" tests/docker >/dev/null
docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --name "$NAME" --privileged --cgroupns=host \
  -v /sys/fs/cgroup:/sys/fs/cgroup:rw -p "$PORT:8443" "$IMAGE" >/dev/null
for _ in $(seq 1 60); do
  state="$(docker exec "$NAME" systemctl is-system-running 2>/dev/null || true)"
  case "$state" in running|degraded) break ;; esac
  sleep 1
done
pass "systemd is $state"
in_box "bash /fixtures/after-boot.sh" >/dev/null
pass "sample databases created (mysql: shop_prod, portal_db · postgres: api_prod)"

# ------------------------------------------------------------------ install
step "Run the installer"
docker cp dist/nexacloud-x86_64-linux.tar.gz "$NAME:/tmp/"
docker cp dist/nexacloud-x86_64-linux.tar.gz.sha256 "$NAME:/tmp/"
out="$(in_box "NEXACLOUD_TARBALL=/tmp/nexacloud-x86_64-linux.tar.gz bash -s" < scripts/install.sh)"
echo "$out" | sed 's/^/    | /'
PASS="$(printf '%s\n' "$out" | sed -n 's/^  Password:  //p' | head -1)"
[ -n "$PASS" ] && pass "installer printed a password" || fail "no password printed"
check "systemd service is active" in_box "systemctl is-active --quiet nexacloud"
check "config is mode 600" bash -c "[ \"\$(docker exec $NAME stat -c %a /etc/nexacloud/config.toml)\" = 600 ]"

# ------------------------------------------------------------------ auth
step "Auth & request protection"
check "GET /api/health over HTTPS" curl -fsk "$U/api/health"
check "web UI served at /" bash -c "curl -fsk $U/ | grep -qi '<html'"
check "SPA fallback for /databases" bash -c "curl -fsk $U/databases | grep -qi '<html'"
check "API refuses without login (401)" test "$(code "$U/api/system")" = 401
check "wrong password → 401" test "$(code -H "Origin: $U" -H 'Content-Type: application/json' -d '{"password":"nope"}' "$U/api/auth/login")" = 401
check "POST without Origin → 403" test "$(code -H 'Content-Type: application/json' -d "{\"password\":\"$PASS\"}" "$U/api/auth/login")" = 403
check "POST with foreign Origin → 403" test "$(code -H 'Origin: https://evil.example' -H 'Content-Type: application/json' -d "{\"password\":\"$PASS\"}" "$U/api/auth/login")" = 403
JAR="$(mktemp)"
curl -fsSk -c "$JAR" -D /tmp/nc-headers -H "Origin: $U" -H 'Content-Type: application/json' -d "{\"password\":\"$PASS\"}" "$U/api/auth/login" >/dev/null
pass "login with installer password"
check "cookie is HttpOnly + Secure + SameSite=Strict" bash -c "grep -i '^set-cookie' /tmp/nc-headers | grep -qi httponly && grep -i '^set-cookie' /tmp/nc-headers | grep -qi secure && grep -i '^set-cookie' /tmp/nc-headers | grep -qi 'samesite=strict'"

# ------------------------------------------------------------------ read endpoints
step "Real data from the server"
sys="$(api system)"; echo "    system: $(echo "$sys" | jq -c '{os,cpu_count,mem:.memory.total,disks:(.disks|length)}')"
check "system reports Ubuntu 24.04" jqe "$sys" '.os | test("Ubuntu 24.04")'
sites="$(api sites)"; echo "    sites: $(echo "$sites" | jq -c '[.[] | {id,kind,php_version,ssl}]')"
check "shop detected as Laravel with PHP 8.3" jqe "$sites" '.[] | select(.id=="shop.test.local") | .kind=="laravel" and .php_version=="8.3"'
check "shop SSL expiry read from certificate" jqe "$sites" '.[] | select(.id=="shop.test.local") | .ssl==true and (.ssl_expires|type)=="string"'
check "shop git branch + commit" jqe "$sites" '.[] | select(.id=="shop.test.local") | .git.branch=="main" and (.git.message|test("Initial shop"))'
check "redirect block merged (shop listed once)" jqe "$sites" '[.[] | select(.id=="shop.test.local")] | length == 1'
check "portal detected as plain PHP" jqe "$sites" '.[] | select(.id=="portal.test.local") | .kind=="php"'
check "ws detected as proxy" jqe "$sites" '.[] | select(.id=="ws.test.local") | .kind=="proxy"'
php="$(api php)"; echo "    php: $(echo "$php" | jq -c '.')"
check "PHP 8.3 found, used by 2 sites" jqe "$php" '.versions[] | select(.version=="8.3") | (.sites|length)==2 and .fpm_active==true'
svc="$(api services)"; echo "    services: $(echo "$svc" | jq -c '[.[] | select(.installed) | {unit,active}]')"
for u in nginx mysql postgresql php8.3-fpm; do
  check "service $u active" jqe "$svc" '.[] | select(.unit=="'"$u"'") | .active==true'
done
dbs="$(api databases)"; echo "    databases: $(echo "$dbs" | jq -c '{mysql:[.mysql.databases[].name], postgres:[.postgres.databases[].name]}')"
check "MySQL lists shop_prod and portal_db (no system schemas)" jqe "$dbs" '[.mysql.databases[].name] | sort == ["portal_db","shop_prod"]'
check "PostgreSQL lists api_prod" jqe "$dbs" '[.postgres.databases[].name] == ["api_prod"]'
ov="$(api overview)"; echo "    alerts: $(echo "$ov" | jq -c '[.alerts[] | {level,title}]')"
check "alert: firewall off (critical)" jqe "$ov" '[.alerts[] | select(.level=="critical" and .link=="/security")] | length > 0'
check "alert: certificate expiring (warning)" jqe "$ov" '[.alerts[] | select(.link=="/sites")] | length > 0'
check "security endpoint" jqe "$(api security)" '.ufw | has("installed")'
check "jobs endpoint" jqe "$(api jobs)" 'type=="array"'

# ------------------------------------------------------------------ actions
step "Actions"
post() { curl -fsSk -b "$JAR" -H "Origin: $U" -X POST "$U/api/$1"; }
wait_job() {
  local id="$1" st=""
  for _ in $(seq 1 60); do
    st="$(api "jobs/$id" | jq -r .status)"
    case "$st" in ok|failed) break ;; esac
    sleep 1
  done
  echo "$st"
}
for target in mysql/shop_prod postgres/api_prod; do
  jid="$(post "databases/$target/backup" | jq -r .job_id)"
  st="$(wait_job "$jid")"
  [ "$st" = ok ] && pass "backup $target → job ok" || { api "jobs/$jid" | jq -r .log; fail "backup $target ($st)"; }
done
bk="$(api backups)"
f="$(echo "$bk" | jq -r '[.[] | select(.database=="shop_prod")][0].file')"
check "backup listed" test -n "$f"
check "backup file is mode 600" bash -c "[ \"\$(docker exec $NAME stat -c %a /var/backups/nexacloud/$f)\" = 600 ]"
curl -fsSk -b "$JAR" -o /tmp/nc-dump.sql.gz "$U/api/backups/download?file=$(jq -rn --arg f "$f" '$f|@uri')"
check "download is a gzip containing the orders table" bash -c "gunzip -c /tmp/nc-dump.sql.gz | grep -q 'CREATE TABLE \`orders\`'"
check "path traversal refused" test "$(code -b "$JAR" "$U/api/backups/download?file=../../../etc/shadow")" = 400
check "backup of unknown database refused" test "$(code -b "$JAR" -H "Origin: $U" -X POST "$U/api/databases/mysql/nope_db/backup")" != 200
check "bad database name refused" test "$(code -b "$JAR" -H "Origin: $U" -X POST "$U/api/databases/mysql/a;rm%20-rf/backup")" != 200
check "restart nginx" post services/nginx/restart
check "nginx still active after restart" in_box "systemctl is-active --quiet nginx"
check "restart of unknown unit refused (400)" test "$(code -b "$JAR" -H "Origin: $U" -X POST "$U/api/services/ssh/restart")" = 400
check "logout" post auth/logout

# ------------------------------------------------------------------ throttle
step "Login throttling"
for _ in 1 2 3 4 5 6; do code -H "Origin: $U" -H 'Content-Type: application/json' -d '{"password":"bad"}' "$U/api/auth/login" >/dev/null; done
check "6th+ wrong attempt → 429" test "$(code -H "Origin: $U" -H 'Content-Type: application/json' -d '{"password":"bad"}' "$U/api/auth/login")" = 429

# ------------------------------------------------------------------ lifecycle
step "Upgrade, reset password, uninstall"
up="$(in_box "NEXACLOUD_TARBALL=/tmp/nexacloud-x86_64-linux.tar.gz bash -s" < scripts/install.sh)"
check "re-run keeps existing password" grep -q 'Password:  unchanged' <<<"$up"
check "agent healthy after upgrade" curl -fsk "$U/api/health"
check "reset-password prints a new password" bash -c "docker exec $NAME nexacloud reset-password | grep -q '^NEXACLOUD_ADMIN_PASSWORD='"
if [ "${KEEP:-0}" = "1" ]; then
  NEWPASS="$(in_box 'nexacloud reset-password' | sed -n 's/^NEXACLOUD_ADMIN_PASSWORD=//p')"
  in_box "systemctl restart nexacloud"
  printf '\n\033[1mAll %d checks passed.\033[0m Container kept running:\n  %s   password: %s\n' "$PASSES" "$U" "$NEWPASS"
  exit 0
fi
in_box "bash -s -- --uninstall" < scripts/install.sh >/dev/null
check "service removed" bash -c "! docker exec $NAME systemctl is-active --quiet nexacloud"
check "/opt/nexacloud removed" bash -c "! docker exec $NAME test -e /opt/nexacloud"
check "backups kept after uninstall" in_box "test -d /var/backups/nexacloud"
check "sites untouched (nginx still serves shop)" bash -c "docker exec $NAME curl -fsk -H 'Host: shop.test.local' https://127.0.0.1/ | grep -q 'shop ok'"

printf '\n\033[1;32mAll %d checks passed.\033[0m\n' "$PASSES"
