#!/usr/bin/env bash
# End-to-end test: build a Linux package, boot an Ubuntu 24.04 "VPS" container
# with systemd + nginx + PHP + MySQL + PostgreSQL, run the real installer and
# exercise every API endpoint, then upgrade, reset password and uninstall.
#
#   bash tests/docker/run-e2e.sh            # full run, removes the container at the end
#   KEEP=1 bash tests/docker/run-e2e.sh     # leave the container running (panel on https://127.0.0.1:18445)
#   SKIP_BUILD=1 ...                        # reuse dist/ from a previous run
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
NAME=nexacloud-e2e
IMAGE=nexacloud-e2e:24.04
# 18443 is used by the long-running demo container; keep e2e separate.
PORT="${PORT:-18445}"
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
# POST with a JSON body; prints the response body (fails on HTTP errors).
post_json() { curl -fsSk -b "$JAR" -H "Origin: $U" -H 'Content-Type: application/json' -d "$2" "$U/api/$1"; }
# POST and print only the status code (body to /tmp/nc-body).
post_code() {
  local body="${2:-}"; [ -n "$body" ] || body='{}'
  curl -sk -o /tmp/nc-body -w '%{http_code}' -b "$JAR" -H "Origin: $U" -H 'Content-Type: application/json' -d "$body" "$U/api/$1"
}
sudo_on() { post_json auth/sudo "{\"password\":\"$1\"}" >/dev/null; }
login() { curl -fsSk -c "$JAR" -H "Origin: $U" -H 'Content-Type: application/json' -d "{\"password\":\"$1\"}" "$U/api/auth/login" >/dev/null; }
# Value of the Server header nginx sends for a sample site (inside the box).
server_header() { in_box "curl -sI -H 'Host: portal.test.local' http://127.0.0.1/ | tr -d '\r' | sed -n 's/^[Ss]erver: //p'"; }
wait_health() { for _ in $(seq 1 30); do curl -fsk "$U/api/health" >/dev/null 2>&1 && return 0; sleep 1; done; return 1; }

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
check "data dir /var/lib/nexacloud is mode 700" bash -c "[ \"\$(docker exec $NAME stat -c %a /var/lib/nexacloud)\" = 700 ]"
check "state database is mode 600" bash -c "[ \"\$(docker exec $NAME stat -c %a /var/lib/nexacloud/state.db)\" = 600 ]"

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
    case "$st" in ok|failed|cancelled|interrupted) break ;; esac
    sleep 1
  done
  echo "$st"
}
for target in mysql/shop_prod postgres/api_prod; do
  jid="$(post "databases/$target/backup" | jq -r .job_id)"
  st="$(wait_job "$jid")"
  [ "$st" = ok ] && pass "backup $target → job ok" || { api "jobs/$jid" | jq -r .log; fail "backup $target ($st)"; }
  check "backup $target ran as a heavy job holding db:$target" jqe "$(api "jobs/$jid")" '.heavy==true and .locks==["db:'"$target"'"] and (.log|test("Wrote"))'
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
check "restart without sudo → 403 sudo_required" bash -c "[ \"\$(curl -sk -b $JAR -H 'Origin: $U' -X POST $U/api/services/nginx/restart)\" = '{\"error\":\"sudo_required\"}' ]"
check "auth/me: not in sudo mode" jqe "$(api auth/me)" '.ok==true and .sudo_until==null'
check "wrong sudo password → 401" test "$(post_code auth/sudo '{"password":"nope"}')" = 401
check "sudo with the admin password" sudo_on "$PASS"
check "auth/me reports sudo_until" jqe "$(api auth/me)" '.sudo_until|type=="string"'
check "restart nginx (sudo)" post services/nginx/restart
check "nginx still active after restart" in_box "systemctl is-active --quiet nginx"
check "restart of unknown unit refused (400)" test "$(code -b "$JAR" -H "Origin: $U" -X POST "$U/api/services/ssh/restart")" = 400
check "logout" post auth/logout

# ------------------------------------------------------------------ phase 0: change pipeline
step "Change pipeline: hide nginx version (preview → apply → verify)"
login "$PASS"
check "test hooks are off by default (404)" test "$(post_code test-hooks/sleep-job '{"secs":1}')" = 404
check "apply without sudo → 403 sudo_required" bash -c "[ \"\$(curl -sk -b $JAR -H 'Origin: $U' -X POST $U/api/advisor/nginx-hardening/apply)\" = '{\"error\":\"sudo_required\"}' ]"
sv="$(server_header)"; echo "    Server: $sv"
check "nginx shows its version before" grep -q '^nginx/[0-9]' <<<"$sv"
pv="$(post_json advisor/nginx-hardening/preview '{}')"
echo "    preview: $(echo "$pv" | jq -c '{nothing_to_do, files: [.files[] | {path, action, mode}], commands}')"
check "preview plans one new drop-in file" jqe "$pv" '.nothing_to_do==false and (.files|length)==1 and .files[0].path=="/etc/nginx/conf.d/zz-nexacloud.conf" and .files[0].action=="create"'
check "preview diff adds server_tokens off" jqe "$pv" '.files[0].diff | test("\\+server_tokens off;")'
check "preview runs nginx -t, then reload" jqe "$pv" '.commands==[["nginx","-t"],["systemctl","reload","nginx"]]'
check "preview wrote nothing" in_box "! test -e /etc/nginx/conf.d/zz-nexacloud.conf"
sudo_on "$PASS"
check "apply with a stale plan_hash → 409" test "$(post_code advisor/nginx-hardening/apply '{"plan_hash":"stale"}')" = 409
ap="$(post_json advisor/nginx-hardening/apply "{\"plan_hash\":\"$(jq -r .plan_hash <<<"$pv")\"}")"
CID="$(jq -r .change_id <<<"$ap")"; JID="$(jq -r .job_id <<<"$ap")"
st="$(wait_job "$JID")"
[ "$st" = ok ] && pass "apply job ok" || { api "jobs/$JID" | jq -r .log; fail "apply job ($st)"; }
check "change is applied" jqe "$(api "changes/$CID")" '.status=="applied" and .can_rollback==true and .kind=="nginx_hardening"'
check "drop-in exists (mode 644, server_tokens off)" in_box "test \"\$(stat -c %a /etc/nginx/conf.d/zz-nexacloud.conf)\" = 644 && grep -q '^server_tokens off;' /etc/nginx/conf.d/zz-nexacloud.conf"
check "nginx -t passes" in_box "nginx -t"
sv="$(server_header)"; echo "    Server: $sv"
check "curl -I no longer shows the nginx version" test "$sv" = nginx
check "snapshot manifest saved" in_box "test -f /var/lib/nexacloud/snapshots/$CID/manifest.json"
check "job log file written (mode 600)" bash -c "[ \"\$(docker exec $NAME stat -c %a /var/lib/nexacloud/jobs/$JID.log)\" = 600 ]"
check "change detail: diff, commands, passed checks" jqe "$(api "changes/$CID")" '(.files[0].diff|test("server_tokens")) and (.commands|length)==2 and (.verify_results|length)==1 and .verify_results[0].ok and .job_id=="'"$JID"'"'
check "changes list: newest first" jqe "$(api changes)" '.[0].id=="'"$CID"'"'
check "preview now says nothing to do" jqe "$(post_json advisor/nginx-hardening/preview '{}')" '.nothing_to_do==true'
au="$(api 'audit?limit=50')"
check "audit: change.apply by the client IP" jqe "$au" '[.[] | select(.change_id=="'"$CID"'" and .action=="change.apply" and .result=="ok" and (.ip|type)=="string" and (.plan_hash|length)==64)] | length==1'
check "audit: change.applied outcome" jqe "$au" '[.[] | select(.change_id=="'"$CID"'" and .action=="change.applied")] | length==1'
check "audit: login, sudo and restart recorded" jqe "$au" '([.[].action] | index("auth.login") != null) and ([.[].action] | index("auth.sudo") != null) and ([.[] | select(.action=="service.restart" and .target=="nginx")] | length >= 1)'
sse="$(curl -skN --max-time 15 -b "$JAR" -D /tmp/nc-sse-h "$U/api/jobs/$JID/stream" || true)"
check "SSE: text/event-stream" grep -qi '^content-type: *text/event-stream' /tmp/nc-sse-h
check "SSE replays the job log" grep -q '^data: .*Snapshot saved' <<<"$sse"
last="$(grep -A1 '^event: status' <<<"$sse" | tail -1)"
check "SSE ends with a final status event" grep -q '"status":"ok"' <<<"$last"

step "Rollback (API) and drift protection"
JAR2="$(mktemp)"
curl -fsSk -c "$JAR2" -H "Origin: $U" -H 'Content-Type: application/json' -d "{\"password\":\"$PASS\"}" "$U/api/auth/login" >/dev/null
check "rollback from a session without sudo → 403 sudo_required" bash -c "[ \"\$(curl -sk -b $JAR2 -H 'Origin: $U' -X POST $U/api/changes/$CID/rollback)\" = '{\"error\":\"sudo_required\"}' ]"
rm -f "$JAR2"
rb="$(post_json "changes/$CID/rollback" '{}')"
RCID="$(jq -r .change_id <<<"$rb")"; st="$(wait_job "$(jq -r .job_id <<<"$rb")")"
check "rollback job ok" test "$st" = ok
check "drop-in removed" in_box "! test -e /etc/nginx/conf.d/zz-nexacloud.conf"
check "nginx -t passes after rollback" in_box "nginx -t"
sv="$(server_header)"; echo "    Server: $sv"
check "nginx version visible again" grep -q '^nginx/[0-9]' <<<"$sv"
check "original change is rolled_back" jqe "$(api "changes/$CID")" '.status=="rolled_back" and .rolled_back_by=="'"$RCID"'" and .can_rollback==false'
check "rollback recorded as its own change" jqe "$(api "changes/$RCID")" '.kind=="rollback" and .rollback_of=="'"$CID"'" and .status=="applied"'
check "rolling back twice → 409" test "$(post_code "changes/$CID/rollback")" = 409
ap="$(post_json advisor/nginx-hardening/apply '{}')"; CID2="$(jq -r .change_id <<<"$ap")"
check "re-apply ok" test "$(wait_job "$(jq -r .job_id <<<"$ap")")" = ok
in_box "cp /etc/nginx/conf.d/zz-nexacloud.conf /tmp/zz.bak && echo '# edited by hand' >> /etc/nginx/conf.d/zz-nexacloud.conf"
check "rollback refuses a file edited since (409)" test "$(post_code "changes/$CID2/rollback")" = 409
check "…and says which file" grep -q 'zz-nexacloud.conf' /tmp/nc-body
in_box "cp /tmp/zz.bak /etc/nginx/conf.d/zz-nexacloud.conf"

step "Automatic revert, jobs, SSE, cancel (test hooks)"
in_box "mkdir -p /etc/systemd/system/nexacloud.service.d && printf '[Service]\nEnvironment=NEXACLOUD_TEST_HOOKS=1\n' > /etc/systemd/system/nexacloud.service.d/test-hooks.conf && systemctl daemon-reload && systemctl restart nexacloud"
wait_health || fail "agent did not come back"
login "$PASS"
check "cancel without sudo → 403 sudo_required" test "$(post_code jobs/00000000-0000-0000-0000-000000000000/cancel)" = 403
sudo_on "$PASS"
bc="$(post_json test-hooks/broken-change '{}')"; BCID="$(jq -r .change_id <<<"$bc")"; BJID="$(jq -r .job_id <<<"$bc")"
check "broken change: job failed" test "$(wait_job "$BJID")" = failed
check "broken change auto-reverted" jqe "$(api "changes/$BCID")" '.status=="reverted" and (.error|test("nginx -t"))'
check "broken file removed again" in_box "! test -e /etc/nginx/conf.d/zz-nexacloud-test-broken.conf"
check "nginx -t passes, nginx active" in_box "nginx -t && systemctl is-active --quiet nginx"
check "job log shows the restore" grep -q 'Restored /etc/nginx/conf.d/zz-nexacloud-test-broken.conf' <<<"$(api "jobs/$BJID" | jq -r .log)"
check "audit: change.reverted" jqe "$(api 'audit?limit=50')" '[.[] | select(.change_id=="'"$BCID"'" and .action=="change.reverted")] | length==1'
S="$(post_json test-hooks/sleep-job '{"secs":3}' | jq -r .job_id)"
sse="$(curl -skN --max-time 20 -b "$JAR" "$U/api/jobs/$S/stream" || true)"
check "SSE follows a running job live (tick 1..3)" bash -c "grep -q '^data: .*tick 1' <<<\"\$1\" && grep -q '^data: .*tick 3' <<<\"\$1\"" _ "$sse"
last="$(grep -A1 '^event: status' <<<"$sse" | tail -1)"
check "SSE final status ok" grep -q '"status":"ok"' <<<"$last"
C="$(post_json test-hooks/sleep-job '{"secs":45}' | jq -r .job_id)"
sleep 2
check "cancel a running job" post_json "jobs/$C/cancel" '{}'
check "job is cancelled" test "$(wait_job "$C")" = cancelled
check "its process group was killed" bash -c "! docker exec $NAME pgrep -f 'seq 1 45'"
A="$(post_json test-hooks/sleep-job '{"secs":120,"heavy":true}' | jq -r .job_id)"
B="$(post_json test-hooks/sleep-job '{"secs":120,"heavy":true}' | jq -r .job_id)"
sleep 2
check "heavy job 1 running" jqe "$(api "jobs/$A")" '.status=="running"'
check "heavy job 2 queued behind it" jqe "$(api "jobs/$B")" '.status=="queued"'
in_box "systemctl restart nexacloud"; wait_health || fail "agent did not come back"
login "$PASS"
check "running job → interrupted after restart" jqe "$(api "jobs/$A")" '.status=="interrupted"'
check "queued job → interrupted after restart" jqe "$(api "jobs/$B")" '.status=="interrupted"'
check "jobs survive restarts (history kept)" jqe "$(api "jobs/$S")" '.status=="ok" and (.log|test("tick 3"))'

step "CLI: doctor and rollback with the service stopped"
doc="$(in_box 'nexacloud doctor' 2>&1)" && pass "nexacloud doctor exits 0" || { echo "$doc" | sed 's/^/    | /'; fail "nexacloud doctor"; }
echo "$doc" | sed 's/^/    | /'
in_box "systemctl stop nexacloud"
out="$(in_box 'nexacloud rollback --last' 2>&1)" && pass "nexacloud rollback --last (service stopped)" || { echo "$out" | sed 's/^/    | /'; fail "nexacloud rollback --last"; }
echo "$out" | sed 's/^/    | /'
check "drop-in removed by the CLI rollback" in_box "! test -e /etc/nginx/conf.d/zz-nexacloud.conf"
check "nginx version visible again" grep -q '^nginx/[0-9]' <<<"$(server_header)"
check "--last with nothing left → exit 1" bash -c "! docker exec $NAME nexacloud rollback --last"
in_box "rm -f /etc/systemd/system/nexacloud.service.d/test-hooks.conf && rmdir /etc/systemd/system/nexacloud.service.d && systemctl daemon-reload && systemctl start nexacloud"
wait_health || fail "agent did not come back"
login "$PASS"
check "CLI rollback recorded (original rolled_back)" jqe "$(api "changes/$CID2")" '.status=="rolled_back"'
check "rollback change by cli" jqe "$(api changes)" '[.[] | select(.rollback_of=="'"$CID2"'" and .actor_ip=="cli" and .status=="applied")] | length==1'
check "audit: CLI rollback" jqe "$(api 'audit?limit=100')" '[.[] | select(.action=="change.rollback" and .ip=="cli")] | length==1'
check "test hooks off again (404)" test "$(post_code test-hooks/sleep-job '{"secs":1}')" = 404
check "logout" post auth/logout

# ------------------------------------------------------------------ mysql root password
step "MySQL root protected by a password"
login "$PASS"
in_box "mysql -e \"ALTER USER 'root'@'localhost' IDENTIFIED WITH caching_sha2_password BY 'Root-Pass-123'\""
dbs="$(api databases)"
check "panel reports MySQL unreachable (not 'not installed')" jqe "$dbs" '.mysql.available==false and (.mysql.error|test("my.cnf"))'
in_box "printf '[client]\nuser=root\npassword=Root-Pass-123\n' > /root/.my.cnf && chmod 600 /root/.my.cnf"
check "with /root/.my.cnf MySQL is listed again" jqe "$(api databases)" '.mysql.available==true and (.mysql.databases|length)==2'

# ------------------------------------------------------------------ reset password
step "Reset password (no manual restart)"
NEWPASS="$(in_box 'nexacloud reset-password 2>/dev/null' | sed -n 's/^NEXACLOUD_ADMIN_PASSWORD=//p')"
for _ in $(seq 1 30); do curl -fsk "$U/api/health" >/dev/null 2>&1 && break; sleep 1; done
check "old session signed out" test "$(code -b "$JAR" "$U/api/auth/me")" = 401
check "old password refused" test "$(code -H "Origin: $U" -H 'Content-Type: application/json' -d "{\"password\":\"$PASS\"}" "$U/api/auth/login")" = 401
check "new password works immediately" login "$NEWPASS"
PASS="$NEWPASS"

# ------------------------------------------------------------------ Phase 1
step "Phase 1: sites, PHP, services, advisor, backups (all through the change pipeline)"
sudo_on "$PASS"
# plan <kind> <params-json> → applies via preview+apply, waits, echoes change status
plan() {
  local body="{\"kind\":\"$1\",\"params\":$2}" pv hash started cid jid st
  pv="$(post_json plans/preview "$body")" || { echo "preview failed"; return 1; }
  hash="$(printf '%s' "$pv" | jq -r .plan_hash)"
  started="$(post_json plans/apply "{\"kind\":\"$1\",\"params\":$2,\"plan_hash\":\"$hash\"}")" || { echo "apply failed"; return 1; }
  cid="$(printf '%s' "$started" | jq -r .change_id)"; jid="$(printf '%s' "$started" | jq -r .job_id)"
  for _ in $(seq 1 600); do
    st="$(api "changes/$cid" | jq -r .status)"
    case "$st" in applied|reverted|failed|revert_failed) break ;; esac
    sleep 1
  done
  [ "$st" = applied ] || { api "jobs/$jid" | jq -r .log | tail -25 >&2; }
  echo "$st"
}
site_body() { in_box "curl -s -H 'Host: $1' http://127.0.0.1/"; }

st="$(plan site_create '{"kind":"laravel","domain":"blog.test.local","aliases":["www.blog.test.local"],"php_version":"8.3"}')"
check "create Laravel site blog.test.local → applied ($st)" test "$st" = applied
check "new site answers with its placeholder" bash -c "docker exec $NAME curl -s -H 'Host: blog.test.local' http://127.0.0.1/ | grep -q 'blog.test.local is ready'"
check "site listed as managed laravel" jqe "$(api sites)" '.[] | select(.id=="blog.test.local") | .kind=="laravel" and (.config_path|startswith("/etc/nginx/conf.d/nexacloud-"))'
check "site model readable" jqe "$(api sites/blog.test.local/model)" '.domains==["blog.test.local","www.blog.test.local"] and .php_version=="8.3"'
check "duplicate domain refused" test "$(post_code plans/preview '{"kind":"site_create","params":{"kind":"static","domain":"shop.test.local"}}')" = 409
check "path injection refused" test "$(post_code plans/preview '{"kind":"site_create","params":{"kind":"static","domain":"x.test.local","root":"/etc/nginx"}}')" = 409

st="$(plan site_options '{"id":"blog.test.local","options":{"client_max_body_size":"128M","security_headers":true,"hsts":false,"cache_static":true,"block_hidden":true}}')"
check "change site settings → applied" test "$st" = applied
check "nginx config has the new upload limit" in_box "grep -q 'client_max_body_size 128M' /etc/nginx/conf.d/nexacloud-blog.test.local.conf"

broken="$(jq -Rs . <<<'server { this is not valid nginx }')"
st="$(plan nginx_raw "{\"path\":\"/etc/nginx/conf.d/nexacloud-blog.test.local.conf\",\"content\":$broken}")"
check "broken raw nginx edit is reverted automatically ($st)" test "$st" = reverted
check "site still answers after the failed edit" bash -c "docker exec $NAME curl -s -H 'Host: blog.test.local' http://127.0.0.1/ | grep -q 'is ready'"

st="$(plan php_ini '{"version":"8.3","settings":{"memory_limit":"512M","upload_max_filesize":"64M"}}')"
check "php.ini drop-in → applied" test "$st" = applied
check "PHP-FPM 8.3 picked up memory_limit=512M" in_box "php-fpm8.3 -i 2>/dev/null | grep -q 'memory_limit => 512M' || grep -q 'memory_limit = 512M' /etc/php/8.3/fpm/conf.d/99-nexacloud.ini"
check "bad ini value refused" test "$(post_code plans/preview '{"kind":"php_ini","params":{"version":"8.3","settings":{"memory_limit":"1G\nextension=x"}}}')" = 409

st="$(plan php_extensions '{"version":"8.3","extensions":["bcmath","intl"]}')"
check "install PHP extensions bcmath+intl → applied" test "$st" = applied
check "php8.3 -m lists bcmath" in_box "php8.3 -m | grep -qx bcmath"

st="$(plan php_install '{"version":"8.4"}')"
check "install PHP 8.4 from ondrej PPA → applied" test "$st" = applied
check "php8.4-fpm active" in_box "systemctl is-active --quiet php8.4-fpm"
st="$(plan site_php '{"id":"blog.test.local","version":"8.4"}')"
check "switch managed site to PHP 8.4 → applied" test "$st" = applied
check "blog served by php8.4-fpm" in_box "grep -q php8.4-fpm.sock /etc/nginx/conf.d/nexacloud-blog.test.local.conf"
st="$(plan site_php '{"id":"portal.test.local","version":"8.4"}')"
check "switch unmanaged site (portal) to PHP 8.4 → applied" test "$st" = applied
check "portal still answers on 8.4" bash -c "docker exec $NAME curl -s -H 'Host: portal.test.local' http://127.0.0.1/ | grep -q 'portal ok'"
check "PHP page shows 8.4 used by portal" jqe "$(api php)" '.versions[] | select(.version=="8.4") | (.sites|index("portal.test.local"))!=null'

st="$(plan service_install '{"id":"redis"}')"
check "one-click install Redis → applied" test "$st" = applied
check "redis-server active" in_box "systemctl is-active --quiet redis-server"
check "catalog marks redis installed" jqe "$(api catalog)" '.[] | select(.id=="redis") | .installed==true'

adv="$(api advisor)"; echo "    advisor score $(printf '%s' "$adv" | jq .score), findings: $(printf '%s' "$adv" | jq -c '[.findings[].id]')"
check "advisor reports findings with a score" jqe "$adv" '(.score|type)=="number" and (.findings|length)>0'
check "advisor: no backups finding present" jqe "$adv" '[.findings[].id] | index("backups") != null'
st="$(plan advisor_fix '{"rule":"nginx-gzip"}')"
check "advisor fix: gzip for css/js/json → applied" test "$st" = applied
in_box "head -c 5000 /dev/zero | tr '\\000' a > /var/www/shop/public/t.css"
check "nginx now gzips CSS" bash -c "docker exec $NAME curl -sk -H 'Accept-Encoding: gzip' -H 'Host: shop.test.local' -o /dev/null -D - https://127.0.0.1/t.css | grep -qi '^content-encoding: gzip'"
st="$(plan advisor_fix '{"rule":"mysql-buffer-pool"}')"
check "advisor fix: MySQL buffer pool (restarts MySQL) → applied" test "$st" = applied
check "MySQL is running with the new buffer pool" in_box "mysqladmin ping >/dev/null && [ \$(mysql -N -e 'SELECT @@innodb_buffer_pool_size') -ge 134217728 ]"
check "dismiss a finding" post_json advisor/journald-size/dismiss '{"days":7}'

# ---- backups
st_dest="$(post_json backup/destinations '{"name":"Second disk","kind":"local","config":{"path":"/var/backups/offsite"}}')"
LOCAL_ID="$(printf '%s' "$st_dest" | jq -r .id)"
check "add local destination" test -n "$LOCAL_ID"
KEY="$(api backup/ssh-key | jq -r .public_key)"
check "panel SSH key generated" bash -c "printf '%s' '$KEY' | grep -q '^ssh-ed25519 '"
in_box "systemctl enable --now ssh >/dev/null 2>&1; printf '%s\n' '$KEY' > /home/nas/.ssh/authorized_keys; chown nas:nas /home/nas/.ssh/authorized_keys; chmod 600 /home/nas/.ssh/authorized_keys"
SFTP_ID="$(post_json backup/destinations '{"name":"Synology (test)","kind":"sftp","config":{"host":"127.0.0.1","port":22,"user":"nas","path":"/home/nas/backups"}}' | jq -r .id)"
check "add SFTP destination (fake Synology)" test -n "$SFTP_ID"
check "bad SFTP host refused" test "$(post_code backup/destinations '{"name":"x","kind":"sftp","config":{"host":"a;b","user":"nas","path":"/x"}}')" = 400
wait_job2() { local id="$1" s=""; for _ in $(seq 1 600); do s="$(api "jobs/$id" | jq -r .status)"; case "$s" in ok|failed|cancelled|interrupted) break ;; esac; sleep 1; done; echo "$s"; }
jid="$(post_json "backup/destinations/$SFTP_ID/test" '{}' | jq -r .job_id)"; st="$(wait_job2 "$jid")"
[ "$st" = ok ] || api "jobs/$jid" | jq -r .log | tail -20
check "SFTP destination test (installs rclone, pins host key, writes+deletes) → ok" test "$st" = ok
PLAN_ID="$(post_json backup/plans "{\"name\":\"Nightly\",\"destination_id\":\"$SFTP_ID\",\"databases\":[\"all\"],\"paths\":[\"/var/www/shop/public\"],\"schedule\":\"daily 03:00\"}" | jq -r .id)"
check "create backup plan" test -n "$PLAN_ID"
check "bad schedule refused" test "$(post_code backup/plans '{"name":"x","databases":["all"],"schedule":"sometimes"}')" = 400
jid="$(post_json "backup/plans/$PLAN_ID/run" '{}' | jq -r .job_id)"; st="$(wait_job2 "$jid")"
[ "$st" = ok ] || api "jobs/$jid" | jq -r .log | tail -30
check "backup run (3 databases + folder → SFTP) → ok" test "$st" = ok
RUN="$(api "backup/runs?plan=$PLAN_ID" | jq '.[0]')"
check "run recorded with 4 items and a remote copy" jqe "$RUN" '.status=="ok" and (.items|length)==4 and (.remote_path|type)=="string"'
check "files arrived on the fake NAS" in_box "ls /home/nas/backups/nightly/*/ | grep -q 'shop_prod-.*\.sql\.gz'"
RUN_ID="$(printf '%s' "$RUN" | jq -r .id)"
res="$(post_json "backup/runs/$RUN_ID/restore" '{"source":"mysql/shop_prod"}')"
TARGET="$(printf '%s' "$res" | jq -r .target_database)"; st="$(wait_job2 "$(printf '%s' "$res" | jq -r .job_id)")"
check "restore shop_prod into a copy ($TARGET) → ok" test "$st" = ok
check "restored copy has the 2 orders rows; live DB untouched" in_box "[ \$(mysql -N -e 'SELECT COUNT(*) FROM \`$TARGET\`.orders') = 2 ] && [ \$(mysql -N -e 'SELECT COUNT(*) FROM shop_prod.orders') = 2 ]"
check "advisor no longer reports missing backups" jqe "$(api advisor)" '[.findings[].id] | index("backups") == null'
check "disk usage endpoint" jqe "$(api disk)" 'length > 0 and (.[0].bytes|type)=="number"'

step "Database manager (phpMyAdmin-style)"
sudo_on "$PASS"
check "tables of shop_prod include orders" jqe "$(api dbm/mysql/shop_prod/tables)" '[.[].name] | index("orders") != null'
check "structure shows primary key id" jqe "$(api dbm/mysql/shop_prod/tables/orders/structure)" '.primary_key == ["id"]'
check "browse rows (2, paginated)" jqe "$(api 'dbm/mysql/shop_prod/tables/orders/rows?page=1&per=10&sort=id&dir=desc')" '.total == 2 and (.rows|length) == 2 and .rows[0][0] == "2"'
check "search rows" jqe "$(api 'dbm/mysql/shop_prod/tables/orders/rows?q=99')" '.total == 1'
check "SQL console read" jqe "$(post_json dbm/mysql/shop_prod/query '{"sql":"SELECT COUNT(*) AS n FROM orders"}')" '.rows[0][0] == "2" and .read_only == true'
check "read-only console refuses a write" test "$(post_code dbm/mysql/shop_prod/query '{"sql":"DELETE FROM orders WHERE id = 1"}')" = 400
check "multiple statements refused" test "$(post_code dbm/mysql/shop_prod/query '{"sql":"SELECT 1; DROP TABLE orders"}')" = 400
check "edit row by primary key (sudo)" jqe "$(post_json dbm/mysql/shop_prod/tables/orders/rows/update '{"pk":{"id":"1"},"set":{"total":"11.50"}}')" '.affected == 1'
check "row change stored" in_box "[ \$(mysql -N -e 'SELECT total FROM shop_prod.orders WHERE id=1') = 11.50 ]"
check "write mode: DELETE without WHERE needs the db name (428)" test "$(post_code dbm/mysql/$TARGET/query/write '{"sql":"DELETE FROM orders"}')" = 428
WBODY="$(jq -nc --arg t "$TARGET" '{sql: "DELETE FROM orders", confirm: $t}')"
check "write mode with confirmation" jqe "$(post_json "dbm/mysql/$TARGET/query/write" "$WBODY")" '.affected == 2'
NEW="$(post_json dbm/mysql/databases '{"name":"newapp","user":"newapp_user"}')"
NPW="$(printf '%s' "$NEW" | jq -r .password)"
check "create database + user, password returned once" test "${#NPW}" -ge 20
check "new user can log in to its database" in_box "MYSQL_PWD='$NPW' mysql --no-defaults -unewapp_user -h127.0.0.1 newapp -e 'SELECT 1' >/dev/null"
check "users list includes newapp_user" jqe "$(api dbm/mysql/users)" '[.[].user] | index("newapp_user") != null'
check "PostgreSQL tables of api_prod" jqe "$(api dbm/postgres/api_prod/tables)" '[.[].name] | index("events") != null'
check "PostgreSQL rows" jqe "$(api dbm/postgres/api_prod/tables/events/rows)" '.total >= 1 and .rows[0][1] == "hello"'
check "PostgreSQL read-only console" jqe "$(post_json dbm/postgres/api_prod/query '{"sql":"SELECT body FROM events"}')" '.rows[0][0] == "hello"'
check "dbm write without sudo is refused" bash -c "curl -sk -c /tmp/j2 -H 'Origin: $U' -H 'Content-Type: application/json' -d '{\"password\":\"$PASS\"}' $U/api/auth/login >/dev/null && [ \"\$(curl -sk -b /tmp/j2 -H 'Origin: $U' -H 'Content-Type: application/json' -d '{\"sql\":\"SELECT 1\"}' $U/api/dbm/mysql/shop_prod/query/write)\" = '{\"error\":\"sudo_required\"}' ]"

step "Git deploys (local repo, webhook, auto-rollback, polling)"
sudo_on "$PASS"
in_box 'set -e
git config --global init.defaultBranch main
rm -rf /srv/git /tmp/appsrc && mkdir -p /srv/git /tmp/appsrc/public
git init -q --bare /srv/git/app.git
cd /tmp/appsrc && git init -q && git config user.email dev@example.com && git config user.name Dev
echo "<?php echo \"app v1\";" > public/index.php && git add -A && git commit -qm "v1" && git remote add origin /srv/git/app.git && git push -q origin main
chown -R www-data:www-data /srv/git'
push_version() { printf '%s\n' "$2" | docker exec -i "$NAME" bash -c 'cat > /tmp/appsrc/public/index.php'; in_box "set -e; cd /tmp/appsrc && git commit -qam '$1' && chown -R root /srv/git && git push -q origin main && chown -R www-data:www-data /srv/git"; }
st="$(plan site_create '{"kind":"php","domain":"app.test.local","php_version":"8.3"}')"
check "create site app.test.local → applied" test "$st" = applied
PROJ="$(post_json projects '{"site_id":"app.test.local","repo":"/srv/git/app.git","branch":"main","migrate":false}')"
check "connect site to repository" jqe "$PROJ" '.id=="app.test.local" and .kind=="php" and .dir=="/var/www/app.test.local" and .run_as=="www-data"'
check "bad repository refused" test "$(post_code projects '{"site_id":"shop.test.local","repo":"--upload-pack=touch /tmp/pwn"}')" = 400
deploy_wait() { local jid="$1" s; s="$(wait_job2 "$jid")"; echo "$s"; }
jid="$(post_json projects/app.test.local/deploy '{}' | jq -r .job_id)"; st="$(deploy_wait "$jid")"
[ "$st" = ok ] || api "jobs/$jid" | jq -r .log | tail -30
check "first deploy (git init + fetch + checkout as www-data) → ok" test "$st" = ok
check "site serves v1" bash -c "docker exec $NAME curl -s -H 'Host: app.test.local' http://127.0.0.1/ | grep -qx 'app v1'"
check "checkout owned by www-data (not root)" in_box "[ \$(stat -c %U /var/www/app.test.local/.git) = www-data ]"
SECRET="$(api projects/app.test.local | jq -r .webhook_secret)"
push_version v2 '<?php echo "app v2";'
BODY='{"ref":"refs/heads/main","after":"x"}'
SIG="sha256=$(printf '%s' "$BODY" | openssl dgst -sha256 -hmac "$SECRET" | sed 's/^.* //')"
check "webhook with a bad signature → 401" test "$(curl -sk -o /dev/null -w '%{http_code}' -H 'X-GitHub-Event: push' -H 'X-Hub-Signature-256: sha256=00' -d "$BODY" "$U/hooks/github/app.test.local")" = 401
HOOK="$(curl -sk -H 'Content-Type: application/json' -H 'X-GitHub-Event: push' -H "X-Hub-Signature-256: $SIG" -d "$BODY" "$U/hooks/github/app.test.local")"
jid="$(printf '%s' "$HOOK" | jq -r .job_id)"; st="$(deploy_wait "$jid")"
check "signed GitHub webhook triggers a deploy → ok" test "$st" = ok
check "site serves v2" bash -c "docker exec $NAME curl -s -H 'Host: app.test.local' http://127.0.0.1/ | grep -qx 'app v2'"
push_version v3-broken '<?php http_response_code(500); echo "broken";'
jid="$(post_json projects/app.test.local/deploy '{}' | jq -r .job_id)"; st="$(deploy_wait "$jid")"
check "broken deploy fails its health check" test "$st" = failed
check "…and was rolled back automatically (site serves v2 again)" bash -c "docker exec $NAME curl -s -H 'Host: app.test.local' http://127.0.0.1/ | grep -qx 'app v2'"
check "deploy history shows rolled_back" jqe "$(api projects/app.test.local/deploys)" '.[0].status=="rolled_back" and .[1].status=="ok" and .[1].trigger=="webhook"'
push_version v4 '<?php echo "app v4";'
curl -fsSk -b "$JAR" -H "Origin: $U" -H 'Content-Type: application/json' -X PUT -d '{"auto_deploy":true}' "$U/api/projects/app.test.local" >/dev/null
ok=0; for _ in $(seq 1 150); do docker exec $NAME curl -s -H 'Host: app.test.local' http://127.0.0.1/ | grep -qx 'app v4' && { ok=1; break; }; sleep 1; done
check "auto-deploy polling picked up v4 within 2.5 minutes" test "$ok" = 1
st="$(plan project_env '{"id":"app.test.local","set":{"APP_ENV":"production","GREETING":"hello world"}}')"
check "edit .env through the change pipeline → applied" test "$st" = applied
check ".env has the new values" in_box "grep -qx 'GREETING=\"hello world\"' /var/www/app.test.local/.env"
check "project status endpoint" jqe "$(api projects/app.test.local/status)" '.worker==false and (.artisan|length)>5'

step "Phase 3: FrankenPHP runtime and firewall with anti-lockout guard"
sudo_on "$PASS"
st="$(plan site_runtime '{"id":"app.test.local","runtime":"frankenphp"}')"
check "switch app.test.local to FrankenPHP (starts unit) → applied ($st)" test "$st" = applied
check "FrankenPHP unit is running" in_box "systemctl is-active --quiet nexacloud-frankenphp-app-test-local"
check "site still serves the app through FrankenPHP" bash -c "docker exec $NAME curl -s -H 'Host: app.test.local' http://127.0.0.1/ | grep -qx 'app v4'"
check "/index.php is executed, never served as source" bash -c "! docker exec $NAME curl -s -H 'Host: app.test.local' http://127.0.0.1/index.php | grep -q '<?php'"
check "nginx proxies PHP to FrankenPHP (no fastcgi)" in_box "grep -q 'proxy_pass http://127.0.0.1:81' /etc/nginx/conf.d/nexacloud-app.test.local.conf && ! grep -q fastcgi_pass /etc/nginx/conf.d/nexacloud-app.test.local.conf"
st="$(plan site_runtime '{"id":"app.test.local","runtime":"fpm"}')"
check "switch back to PHP-FPM → applied" test "$st" = applied
check "FrankenPHP unit removed" in_box "! systemctl is-active --quiet nexacloud-frankenphp-app-test-local && [ ! -e /etc/systemd/system/nexacloud-frankenphp-app-test-local.service ]"
check "site serves via PHP-FPM again" bash -c "docker exec $NAME curl -s -H 'Host: app.test.local' http://127.0.0.1/ | grep -qx 'app v4'"

st="$(plan firewall_enable '{"extra_ports":[]}')"
if [ "$st" = applied ]; then
  pass "turn on UFW (SSH limited, 80, 443, panel) → applied"
  check "UFW is active" in_box "ufw status | grep -q '^Status: active'"
  check "anti-lockout guard timer is armed" jqe "$(api firewall/guard)" '.armed == true'
  check "panel still reachable through the firewall" curl -fsk "$U/api/health"
  check "confirm keeps the firewall on (guard disarmed)" bash -c "curl -fsSk -b $JAR -H 'Origin: $U' -X POST $U/api/firewall/confirm >/dev/null && [ \"\$(curl -fsSk -b $JAR $U/api/firewall/guard | jq -r .armed)\" = false ]"
  in_box "ufw --force disable >/dev/null"
else
  echo "    (UFW could not be enabled inside this container: $st — kernel/iptables restriction; the change was reverted)"
  check "failed firewall change was reverted (UFW inactive)" in_box "! ufw status 2>/dev/null | grep -q '^Status: active'"
fi

sudo_on "$PASS"
step "Security fixes (Fable review): console escapes, adopt existing sites"
sudo_on "$PASS"
post_code dbm/mysql/shop_prod/query '{"sql":"SELECT 1 \\! touch /tmp/pwned_mysql"}' >/dev/null
check "MySQL console: \\! shell escape does not run" in_box "[ ! -e /tmp/pwned_mysql ]"
check "PostgreSQL console: backslash meta-command refused" test "$(post_code dbm/postgres/api_prod/query '{"sql":"SELECT 1 \\! touch /tmp/pwned_pg"}')" = 400
check "PostgreSQL console: quote trick to escape read-only refused" test "$(post_code dbm/postgres/api_prod/query "$(jq -nc '{sql: "SELECT '"'"'\\'"'"'; SET default_transaction_read_only=off; DROP TABLE events"}')")" = 400
check "PostgreSQL console: read-only role cannot create tables" test "$(post_code dbm/postgres/api_prod/query '{"sql":"CREATE TABLE pwn (a int)"}')" = 400
check "events table still there" in_box "runuser -u postgres -- psql -Atc 'SELECT count(*) FROM events' -d api_prod >/dev/null"
check "COPY ... TO PROGRAM refused" test "$(post_code dbm/postgres/api_prod/query "$(jq -nc '{sql: "COPY events TO PROGRAM '"'"'id'"'"'"}')")" = 400

st="$(plan site_adopt '{"id":"shop.test.local","accept_custom":true}')"
check "adopt existing HTTPS Laravel site shop.test.local → applied ($st)" test "$st" = applied
check "shop is now managed (conf.d) and keeps its certificate" in_box "grep -q 'ssl_certificate /etc/nginx/ssl/shop.crt' /etc/nginx/conf.d/nexacloud-shop.test.local.conf"
check "old sites-enabled symlink disabled, original file kept" in_box "[ ! -e /etc/nginx/sites-enabled/shop.test.local ] && [ -f /etc/nginx/sites-available/shop.test.local ]"
check "shop still serves over HTTPS after adopt" bash -c "docker exec $NAME curl -sk -H 'Host: shop.test.local' https://127.0.0.1/ | grep -q 'shop ok'"
check "adopted model detected Laravel + PHP" jqe "$(api sites/shop.test.local/model)" '.kind=="laravel" and (.php_version|type)=="string" and .ssl==true'
CID="$(api 'changes?limit=5' | jq -r '[.[] | select(.kind=="site_adopt")][0].id')"
check "roll back the adopt" bash -c "curl -fsSk -b $JAR -H 'Origin: $U' -X POST $U/api/changes/$CID/rollback >/dev/null"
for _ in $(seq 1 30); do in_box "[ -L /etc/nginx/sites-enabled/shop.test.local ]" 2>/dev/null && break; sleep 1; done
check "rollback restored the original symlink and removed the managed conf" in_box "[ -L /etc/nginx/sites-enabled/shop.test.local ] && [ ! -e /etc/nginx/conf.d/nexacloud-shop.test.local.conf ]"
check "shop serves over HTTPS after rollback" bash -c "docker exec $NAME curl -sk -H 'Host: shop.test.local' https://127.0.0.1/ | grep -q 'shop ok'"

st="$(plan site_delete '{"id":"blog.test.local"}')"
check "remove site → applied" test "$st" = applied
gone=0; for _ in $(seq 1 10); do docker exec $NAME curl -s -H 'Host: blog.test.local' http://127.0.0.1/ | grep -q 'is ready' || { gone=1; break; }; sleep 1; done
check "blog no longer served (files kept)" bash -c "[ $gone = 1 ] && docker exec $NAME test -f /var/www/blog.test.local/public/index.php"


# ------------------------------------------------------------------ v0.4
step "Monitoring and alerts (fake Telegram + SMTP inside the box)"
# Check every 5 s and send Telegram to the fake API (test-only settings).
in_box "mkdir -p /etc/systemd/system/nexacloud.service.d && printf '[Service]\nEnvironment=NEXACLOUD_MONITOR_INTERVAL=5\nEnvironment=NEXACLOUD_TELEGRAM_API=http://127.0.0.1:8099\n' > /etc/systemd/system/nexacloud.service.d/monitor.conf && systemctl daemon-reload && systemctl restart nexacloud"
wait_health || fail "agent did not come back"
login "$PASS"
sudo_on "$PASS"
docker exec -d "$NAME" python3 /fixtures/fakes.py
sleep 12
check "every site is probed (shop.test.local answers)" jqe "$(api monitor)" '[.[] | select(.site=="shop.test.local")][0].last_code == 200'
check "per-site stats have 24 hourly buckets" jqe "$(api monitor/shop.test.local)" '(.buckets|length)==24 and .checks>=1 and .uptime>0'
NS='{"telegram":{"enabled":true,"bot_token":"123:good","chat_id":"-100123"},"email":{"enabled":true,"smtp_host":"127.0.0.1","smtp_port":2525,"security":"none","user":"","password":"","from":"alerts@example.com","to":"ops@example.com"},"events":{"site_down":true,"deploy_failed":true,"backup_failed":true,"server":false}}'
check "save alert channels" bash -c "curl -fsSk -b $JAR -H 'Origin: $U' -H 'Content-Type: application/json' -X PUT -d '$NS' $U/api/notify/settings >/dev/null"
check "bot token is never sent back to the browser" jqe "$(api notify/settings)" '.telegram.bot_token=="" and .telegram.has_token==true'
check "test message delivered to Telegram and email" jqe "$(post_json notify/test '{}')" 'length==2 and all(.[]; .ok)'
check "fake Telegram got it (chat id + text)" in_box "grep -q 'chat=-100123 text=.Test notification' /tmp/fake-telegram.log"
check "fake SMTP got it (subject)" in_box "grep -q 'Subject: \[NexaCloud\] Test notification' /tmp/fake-smtp.log"
check "saving with an empty token keeps the old one" bash -c "curl -fsSk -b $JAR -H 'Origin: $U' -H 'Content-Type: application/json' -X PUT -d '$(printf '%s' "$NS" | sed 's/123:good//')' $U/api/notify/settings >/dev/null && curl -fsSk -b $JAR $U/api/notify/settings | jq -e '.telegram.has_token' >/dev/null"
st="$(plan site_create '{"kind":"php","domain":"down.test.local","php_version":"8.3"}')"
check "create down.test.local → applied" test "$st" = applied
in_box "printf '<?php http_response_code(503); echo \"down\";' > /var/www/down.test.local/public/index.php"
ok=0; for _ in $(seq 1 60); do api notifications | jq -e '.[] | select(.title=="down.test.local is down")' >/dev/null && { ok=1; break; }; sleep 1; done
check "3 failed checks → 'site is down' alert" test "$ok" = 1
check "…delivered to Telegram" bash -c "for i in \$(seq 1 10); do docker exec $NAME grep -q 'down.test.local is down' /tmp/fake-telegram.log && exit 0; sleep 1; done; exit 1"
in_box "printf '<?php echo \"up\";' > /var/www/down.test.local/public/index.php"
ok=0; for _ in $(seq 1 30); do api notifications | jq -e '.[] | select(.title=="down.test.local is back up")' >/dev/null && { ok=1; break; }; sleep 1; done
check "recovery alert after the next good check" test "$ok" = 1

step "New app wizard: real Laravel app, own user, MySQL, release folders"
sudo_on "$PASS"
in_box 'set -e
git config --system --add safe.directory "*"
rm -rf /srv/git/crm.git /tmp/crmsrc && git init -q --bare /srv/git/crm.git
cp -a /opt/laravel-skel /tmp/crmsrc && cd /tmp/crmsrc && rm -rf .git .env database/database.sqlite
sed -i "/^\/vendor/d" .gitignore
printf "\nuse Illuminate\\\\Support\\\\Facades\\\\Schedule;\nSchedule::command(\"inspire\")->hourly();\n" >> routes/console.php
printf "\nRoute::get(\"/version\", fn () => \"crm v1\");\n" >> routes/web.php
git init -q -b main && git config user.email dev@example.com && git config user.name Dev
git add -A && git commit -qm "crm v1" && git remote add origin /srv/git/crm.git && git push -q origin main
chmod -R a+rX /srv/git/crm.git'
crm_push() { in_box "set -e; cd /tmp/crmsrc && $2 && git commit -qam '$1' && git push -q origin main && chmod -R a+rX /srv/git/crm.git"; }
crm_get() { in_box "curl -s -H 'Host: crm.test.local' http://127.0.0.1$1"; }
last_deploy() { local s=""; for _ in $(seq 1 900); do s="$(api projects/crm.test.local/deploys | jq -r '.[0].status // empty')"; case "$s" in ok|failed|rolled_back) break ;; esac; sleep 1; done; echo "$s"; }
check "wizard defaults list PHP 8.3 and MySQL" jqe "$(api apps/defaults)" '(.php_versions|index("8.3"))!=null and .mysql==true'
check "wizard refuses a bad domain" test "$(post_code apps '{"kind":"laravel","domain":"nope","php_version":"8.3","repo":"/srv/git/crm.git"}')" = 400
APP="$(post_json apps '{"kind":"laravel","domain":"crm.test.local","php_version":"8.3","repo":"/srv/git/crm.git","branch":"main","database":"mysql","isolate":true,"releases":true,"worker":true,"scheduler":true}')"
st="$(wait_job2 "$(printf '%s' "$APP" | jq -r .job_id)")"
[ "$st" = ok ] || api "jobs/$(printf '%s' "$APP" | jq -r .job_id)" | jq -r .log | tail -30
check "wizard job (site, user, database, .env, project) → ok" test "$st" = ok
st="$(last_deploy)"
[ "$st" = ok ] || api "jobs/$(api projects/crm.test.local/deploys | jq -r '.[0].job_id')" | jq -r .log | tail -40
check "first deploy of the real Laravel app → ok" test "$st" = ok
check "site runs as its own user nc_crm_test_local (folder 750)" in_box "[ \"\$(stat -c '%U %a' /var/www/crm.test.local)\" = 'nc_crm_test_local 750' ]"
check "own PHP-FPM pool, nginx uses its socket" in_box "test -f /etc/php/8.3/fpm/pool.d/nc_crm_test_local.conf && grep -q 'php8.3-fpm-nc_crm_test_local.sock' /etc/nginx/conf.d/nexacloud-crm.test.local.conf"
check ".env is in shared/, mode 600, owned by the site user" in_box "[ \"\$(stat -c '%U %a' /var/www/crm.test.local/shared/.env)\" = 'nc_crm_test_local 600' ]"
check "other sites' PHP user (www-data) cannot read the .env" in_box "! runuser -u www-data -- cat /var/www/crm.test.local/shared/.env >/dev/null 2>&1"
check ".env has a real APP_KEY and the MySQL user" in_box "grep -Eq '^APP_KEY=\"?base64:' /var/www/crm.test.local/shared/.env && grep -q '^DB_DATABASE=crm_test_local' /var/www/crm.test.local/shared/.env"
check "current -> releases/1, storage linked to shared/" in_box "[ \$(readlink /var/www/crm.test.local/current) = releases/1 ] && [ \$(readlink /var/www/crm.test.local/releases/1/storage) = ../../shared/storage ]"
check "Laravel answers through the release (crm v1)" bash -c "[ \"\$(docker exec $NAME curl -s -H 'Host: crm.test.local' http://127.0.0.1/version)\" = 'crm v1' ]"
check "migrations ran against MySQL (users table exists)" in_box "mysql -N -e 'SHOW TABLES' crm_test_local | grep -qx users"
check "deploy steps recorded (fetch → link → backup → script → switch → health)" jqe "$(api projects/crm.test.local/deploys)" '[.[0].steps[].name] == ["Fix permissions","Fetch code","Link shared files","Back up database","Run deploy script","Switch release","Reload & health check"] and all(.[0].steps[]; .status=="ok")'
ok=0; for _ in $(seq 1 60); do in_box "supervisorctl status 'nexacloud-crm-test-local:*' | grep -q RUNNING" 2>/dev/null && { ok=1; break; }; sleep 1; done
check "queue worker started after the first deploy (Supervisor, RUNNING)" test "$ok" = 1
check "scheduler cron installed as the site user" in_box "grep -q 'nc_crm_test_local cd /var/www/crm.test.local/current' /etc/cron.d/nexacloud-crm-test-local"
OV="$(api 'projects/crm.test.local/overview?fresh=true')"
check "overview: Laravel version, MySQL database, schedule from the app" jqe "$OV" '(.facts.laravel|type)=="string" and .facts.db.database=="crm_test_local" and ([.facts.schedule[].command]|index("inspire"))!=null'
check "overview: worker RUNNING with its queue size" jqe "$OV" '.workers[0].state=="RUNNING" and .workers[0].queue=="default" and (.workers[0].pending|type)=="number"'
check "overview: scheduler on, monitor stats, errors block" jqe "$OV" '.scheduler.enabled==true and (.monitor.buckets|length)==24 and (.errors.items|type)=="array"'
check "logs list storage/logs files and nginx errors" jqe "$(api projects/crm.test.local/logs)" '([.sources[].id]|index("nginx-error"))!=null'
check "reveal one secret .env value (sudo)" jqe "$(post_json projects/crm.test.local/env/reveal '{"key":"DB_PASSWORD"}')" '(.value|length)>=20'
check "env listing still hides it" jqe "$(api projects/crm.test.local/env)" '[.vars[] | select(.key=="DB_PASSWORD")][0].value==null'
check "restart one worker" jqe "$(post_json projects/crm.test.local/workers/restart '{"name":"default"}')" '(.restarted|length)>=1'

crm_push "crm v2" "sed -i 's/crm v1/crm v2/' routes/web.php"
jid="$(post_json projects/crm.test.local/deploy '{}' | jq -r .job_id)"; st="$(wait_job2 "$jid")"
check "deploy v2 → ok (releases/2)" test "$st" = ok
check "v2 is live, current -> releases/2" bash -c "[ \"\$(docker exec $NAME curl -s -H 'Host: crm.test.local' http://127.0.0.1/version)\" = 'crm v2' ] && [ \"\$(docker exec $NAME readlink /var/www/crm.test.local/current)\" = releases/2 ]"
crm_push "crm v3 broken" "sed -i \"s#^Route::get('/', function () {#Route::get('/', function () { abort(500);#\" routes/web.php"
jid="$(post_json projects/crm.test.local/deploy '{}' | jq -r .job_id)"; st="$(wait_job2 "$jid")"
check "broken v3 fails its health check" test "$st" = failed
check "…current switched back to releases/2, releases/3 removed, v2 still served" bash -c "[ \"\$(docker exec $NAME readlink /var/www/crm.test.local/current)\" = releases/2 ] && ! docker exec $NAME test -e /var/www/crm.test.local/releases/3 && [ \"\$(docker exec $NAME curl -s -H 'Host: crm.test.local' http://127.0.0.1/version)\" = 'crm v2' ]"
check "deploy recorded as rolled_back" jqe "$(api projects/crm.test.local/deploys)" '.[0].status=="rolled_back"'
curl -fsSk -b "$JAR" -H "Origin: $U" -H 'Content-Type: application/json' -X PUT -d '{"run_tests":true}' "$U/api/projects/crm.test.local" >/dev/null
jid="$(post_json projects/crm.test.local/deploy '{}' | jq -r .job_id)"; st="$(wait_job2 "$jid")"
check "with 'stop if tests fail' on, the same commit fails" test "$st" = failed
check "…at Run tests, Switch release skipped" jqe "$(api projects/crm.test.local/deploys)" '.[0].steps as $s | ($s | map(select(.name=="Run tests"))[0].status=="failed") and ($s | map(select(.name=="Switch release"))[0].status=="skipped")'
check "…and nothing changed on the live site" bash -c "[ \"\$(docker exec $NAME readlink /var/www/crm.test.local/current)\" = releases/2 ]"
curl -fsSk -b "$JAR" -H "Origin: $U" -H 'Content-Type: application/json' -X PUT -d '{"run_tests":false}' "$U/api/projects/crm.test.local" >/dev/null
check "releases listed (2 current, 1 kept)" jqe "$(api projects/crm.test.local/releases)" '[.releases[].name]==["2","1"] and .releases[0].current==true and (.releases[1].commit|length)==40'
jid="$(post_json projects/crm.test.local/releases/1/activate '{}' | jq -r .job_id)"; st="$(wait_job2 "$jid")"
check "activate release 1 (switch only) → v1 live" bash -c "[ '$st' = ok ] && [ \"\$(docker exec $NAME curl -s -H 'Host: crm.test.local' http://127.0.0.1/version)\" = 'crm v1' ]"
check "rollback pauses auto-deploy and shows a switch-only deploy" jqe "$(api projects/crm.test.local/deploys)" '.[0].trigger=="rollback" and ([.[0].steps[].name]==["Fix permissions","Switch release","Reload & health check"])'
jid="$(post_json projects/crm.test.local/releases/2/activate '{}' | jq -r .job_id)"; st="$(wait_job2 "$jid")"
check "activate release 2 again → v2" bash -c "[ '$st' = ok ] && [ \"\$(docker exec $NAME curl -s -H 'Host: crm.test.local' http://127.0.0.1/version)\" = 'crm v2' ]"
check "deploy failure alert was sent" jqe "$(api notifications)" '[.[] | select(.title=="Deploy of crm.test.local was rolled back")] | length >= 1'

step "Existing site: switch to release folders, then give it its own user"
sudo_on "$PASS"
st="$(plan project_releases '{"id":"app.test.local"}')"
check "convert app.test.local to release folders → applied ($st)" test "$st" = applied
check "current -> releases/1, .env copied to shared/, still serves app v4" bash -c "[ \"\$(docker exec $NAME readlink /var/www/app.test.local/current)\" = releases/1 ] && docker exec $NAME test -f /var/www/app.test.local/shared/.env && docker exec $NAME curl -s -H 'Host: app.test.local' http://127.0.0.1/ | grep -qx 'app v4'"
check "project now reports strategy releases" jqe "$(api projects/app.test.local)" '.strategy=="releases"'
push_version v5 '<?php echo "app v5";'
jid="$(post_json projects/app.test.local/deploy '{}' | jq -r .job_id)"; st="$(wait_job2 "$jid")"
check "deploy v5 into releases/2 → ok" bash -c "[ '$st' = ok ] && [ \"\$(docker exec $NAME readlink /var/www/app.test.local/current)\" = releases/2 ] && docker exec $NAME curl -s -H 'Host: app.test.local' http://127.0.0.1/ | grep -qx 'app v5'"
st="$(plan site_isolate '{"id":"app.test.local"}')"
check "give app.test.local its own user + pool → applied ($st)" test "$st" = applied
check "files owned by nc_app_test_local, nginx uses the new pool" in_box "[ \$(stat -c %U /var/www/app.test.local/releases/2/public/index.php) = nc_app_test_local ] && grep -q 'php8.3-fpm-nc_app_test_local.sock' /etc/nginx/conf.d/nexacloud-app.test.local.conf"
check "site still answers after isolating (v5)" bash -c "docker exec $NAME curl -s -H 'Host: app.test.local' http://127.0.0.1/ | grep -qx 'app v5'"
check "project follows the new user" jqe "$(api projects/app.test.local)" '.run_as=="nc_app_test_local"'
push_version v6 '<?php echo "app v6 as ".posix_getpwuid(posix_geteuid())["name"];'
jid="$(post_json projects/app.test.local/deploy '{}' | jq -r .job_id)"; st="$(wait_job2 "$jid")"
check "deploy as the new user → v6 served by its own pool" bash -c "[ '$st' = ok ] && docker exec $NAME curl -s -H 'Host: app.test.local' http://127.0.0.1/ | grep -qx 'app v6 as nc_app_test_local'"

step "Project page actions: maintenance mode, front controller, .env as text"
sudo_on "$PASS"
OPTS="$(api sites/app.test.local/model | jq -c '.options')"
st="$(plan site_options "{\"id\":\"app.test.local\",\"options\":$(printf '%s' "$OPTS" | jq -c '.maintenance=true')}")"
check "plain PHP maintenance mode on → applied (no probe on purpose)" test "$st" = applied
check "…site answers 503 with the maintenance page" bash -c "for i in \$(seq 1 10); do docker exec $NAME curl -s -H 'Host: app.test.local' http://127.0.0.1/ | grep -q 'right back' && [ \"\$(docker exec $NAME curl -s -o /dev/null -w '%{http_code}' -H 'Host: app.test.local' http://127.0.0.1/)\" = 503 ] && exit 0; sleep 1; done; exit 1"
st="$(plan site_options "{\"id\":\"app.test.local\",\"options\":$(printf '%s' "$OPTS" | jq -c '.maintenance=false | .front_controller=false')}")"
check "maintenance off + front controller off → applied" test "$st" = applied
check "…unknown paths now 404 instead of index.php" bash -c "for i in \$(seq 1 10); do [ \"\$(docker exec $NAME curl -s -o /dev/null -w '%{http_code}' -H 'Host: app.test.local' http://127.0.0.1/nope)\" = 404 ] && exit 0; sleep 1; done; exit 1"
st="$(plan site_options "{\"id\":\"app.test.local\",\"options\":$(printf '%s' "$OPTS" | jq -c '.front_controller=true')}")"
check "front controller back on → /nope served by index.php" bash -c "[ '$st' = applied ] && for i in \$(seq 1 10); do docker exec $NAME curl -s -H 'Host: app.test.local' http://127.0.0.1/nope | grep -q 'app v6' && exit 0; sleep 1; done; exit 1"
RAW="$(post_json projects/crm.test.local/env/raw '{}')"
check ".env as text (sudo) returns the whole file" jqe "$RAW" '(.content|test("DB_PASSWORD="))'
st="$(plan project_env_raw "$(jq -nc --arg c "$(printf '%s' "$RAW" | jq -r .content)
EXTRA_FLAG=on" '{id:"crm.test.local",content:$c}')")"
check "save .env as text → applied" bash -c "[ '$st' = applied ] && docker exec $NAME grep -qx 'EXTRA_FLAG=on' /var/www/crm.test.local/shared/.env"
check "a broken .env line is refused" test "$(post_code plans/preview '{"kind":"project_env_raw","params":{"id":"crm.test.local","content":"not a valid line"}}')" = 409
check "overview reports composer, shared folders and schedule runs" jqe "$(api 'projects/crm.test.local/overview')" '(.composer.lock==true) and ((.shared|index(".env"))!=null) and (.scheduler.runs|type)=="array" and (.monitor.recent|length)==24'

step "Every site is a project; permissions fixed automatically; domains, stats, history"
sudo_on "$PASS"
check "portal.test.local opened as a project without Git" jqe "$(post_json projects '{"site_id":"portal.test.local"}')" '.id=="portal.test.local" and .repo=="" and .needs_key==false'
check "deploying a project without Git is refused (409)" test "$(post_code projects/portal.test.local/deploy '{}')" = 409
check "connect Git later (PUT repo)" jqe "$(curl -fsSk -b $JAR -H "Origin: $U" -H 'Content-Type: application/json' -X PUT -d '{"repo":"/srv/git/app.git","branch":"main"}' $U/api/projects/portal.test.local)" '.repo=="/srv/git/app.git"'
curl -fsSk -b "$JAR" -H "Origin: $U" -H 'Content-Type: application/json' -X PUT -d '{"repo":""}' "$U/api/projects/portal.test.local" >/dev/null
in_box "chown -R root:root /var/www/crm.test.local/shared/storage/logs && chmod 0755 /var/www/crm.test.local/shared/storage/logs"
check "overview reports the broken permissions" jqe "$(api 'projects/crm.test.local/overview')" '.permissions.ok==false and (.permissions.problems|length)>=1 and .permissions.web_user=="nc_crm_test_local"'
st="$(plan project_permissions '{"id":"crm.test.local"}')"
check "Fix permissions → applied" test "$st" = applied
check "…storage/logs writable by the PHP user again" in_box "runuser -u nc_crm_test_local -- test -w /var/www/crm.test.local/shared/storage/logs"
check "…overview is happy" jqe "$(api 'projects/crm.test.local/overview')" '.permissions.ok==true'
check "workspace lists every site with monitoring" jqe "$(api workspace)" '([.[].id]|index("crm.test.local"))!=null and ([.[] | select(.id=="crm.test.local")][0].project.strategy=="releases") and (.[0].monitor|has("uptime"))'
check "domains API: every hostname with DNS + SSL state" jqe "$(api domains)" '(.domains|length)>=5 and ([.domains[] | select(.domain=="shop.test.local")][0] | .ssl==true and (.dns.state|type)=="string")'
check "database stats: connections, hit rate, used-by" jqe "$(api dbm/mysql/stats)" '(.connections|type)=="number" and (.used_by["crm_test_local"]=="crm.test.local") and .local_only==true'
check "server history endpoint (CPU/memory/network)" jqe "$(api system/history)" '(.points|length)==48 and (.net|has("rx_24h"))'

step "Encrypted off-site backups and restore to staging"
sudo_on "$PASS"
ENC="$(post_json backup/destinations "{\"name\":\"NAS encrypted\",\"kind\":\"sftp\",\"config\":{\"host\":\"127.0.0.1\",\"port\":22,\"user\":\"nas\",\"path\":\"/home/nas/enc\",\"encrypt_password\":\"correct horse battery staple\"}}")"
ENC_ID="$(printf '%s' "$ENC" | jq -r .id)"
check "encryption password is never sent back" jqe "$ENC" '.config.encrypted==true and (.config.encrypt_password==null)'
check "short encryption password refused" test "$(post_code backup/destinations '{"name":"x","kind":"sftp","config":{"host":"127.0.0.1","user":"nas","path":"/home/nas/x","encrypt_password":"short"}}')" = 400
st="$(wait_job2 "$(post_json "backup/destinations/$ENC_ID/test" '{}' | jq -r .job_id)")"
check "encrypted destination test → ok" test "$st" = ok
EP="$(post_json backup/plans "{\"name\":\"crm full\",\"destination_id\":\"$ENC_ID\",\"databases\":[\"mysql/crm_test_local\"],\"paths\":[\"/var/www/crm.test.local\"],\"schedule\":\"daily 03:00\",\"keep_daily\":3,\"keep_weekly\":0,\"keep_monthly\":0,\"enabled\":false}")"
EP_ID="$(printf '%s' "$EP" | jq -r .id)"
jid="$(post_json "backup/plans/$EP_ID/run" '{}' | jq -r .job_id)"; st="$(wait_job2 "$jid")"
[ "$st" = ok ] || api "jobs/$jid" | jq -r .log | tail -20
check "encrypted backup run (MySQL + site folder) → ok" test "$st" = ok
check "the NAS only has scrambled names (no .sql.gz / .tar.gz visible)" in_box "test -n \"\$(find /home/nas/enc -type f)\" && ! find /home/nas/enc -name '*.gz' | grep -q ."
ERUN="$(api "backup/runs" | jq -r "[.[] | select(.plan_id==\"$EP_ID\")][0].id")"
in_box "rm -rf /var/backups/nexacloud/runs/$EP_ID"
SB="{\"files\":\"/var/www/crm.test.local\",\"database\":\"mysql/crm_test_local\",\"domain\":\"staging.crm.test.local\"}"
jid="$(post_json "backup/runs/$ERUN/staging" "$SB" | jq -r .job_id)"; st="$(wait_job2 "$jid")"
[ "$st" = ok ] || api "jobs/$jid" | jq -r .log | tail -30
check "restore to staging from the encrypted NAS copy (local copy deleted) → ok" test "$st" = ok
check "staging copy answers (crm v2) on its own domain" bash -c "for i in \$(seq 1 20); do [ \"\$(docker exec $NAME curl -s -H 'Host: staging.crm.test.local' http://127.0.0.1/version)\" = 'crm v2' ] && exit 0; sleep 1; done; exit 1"
check "staging .env points at the copied database, mail to log" in_box "grep -q '^DB_DATABASE=crm_test_local_staging_' /var/www/staging.crm.test.local/shared/.env && grep -q '^MAIL_MAILER=log' /var/www/staging.crm.test.local/shared/.env && grep -q '^APP_ENV=staging' /var/www/staging.crm.test.local/shared/.env"
check "staging database has the data (users table)" in_box "mysql -N -e 'SHOW DATABASES' | grep -q '^crm_test_local_staging_' && mysql -N -e \"SHOW TABLES FROM \$(mysql -N -e 'SHOW DATABASES' | grep '^crm_test_local_staging_')\" | grep -qx users"
check "staging runs as its own user" in_box "[ \$(stat -c %U /var/www/staging.crm.test.local) = nc_staging_crm_test_local ]"
check "live site untouched (crm v2)" bash -c "[ \"\$(docker exec $NAME curl -s -H 'Host: crm.test.local' http://127.0.0.1/version)\" = 'crm v2' ]"
check "staging over an existing folder is refused" test "$(post_code "backup/runs/$ERUN/staging" "$SB")" = 400

st="$(plan site_delete '{"id":"crm.test.local"}')"
check "remove crm.test.local → applied" test "$st" = applied
check "…its pool, worker and cron are cleaned up" in_box "[ ! -e /etc/php/8.3/fpm/pool.d/nc_crm_test_local.conf ] && [ ! -e /etc/supervisor/conf.d/nexacloud-crm-test-local.conf ] && [ ! -e /etc/cron.d/nexacloud-crm-test-local ]"

# ------------------------------------------------------------------ batch A
step "Panel users, roles, API tokens and two-factor sign-in"
H="$(dirname "$0")/helpers"
sudo_on "$PASS"
PORTAL_DIR="$(api projects/portal.test.local | jq -r .dir)"
check "users list has the built-in admin" jqe "$(api users)" '[.[].username]|index("admin")!=null'
check "short password refused" test "$(post_code users '{"username":"dev1","role":"developer","projects":["portal.test.local"],"password":"short"}')" = 400
check "create developer dev1 (portal only)" jqe "$(post_json users '{"username":"dev1","role":"developer","projects":["portal.test.local"],"password":"dev1-password-123"}')" '.ok==true'
check "create viewer view1" jqe "$(post_json users '{"username":"view1","role":"viewer","password":"view1-password-123"}')" '.ok==true'
DJ="$(mktemp)"; VJ="$(mktemp)"
curl -fsSk -c "$DJ" -H "Origin: $U" -H 'Content-Type: application/json' -d '{"username":"dev1","password":"dev1-password-123"}' "$U/api/auth/login" >/dev/null
curl -fsSk -c "$VJ" -H "Origin: $U" -H 'Content-Type: application/json' -d '{"username":"view1","password":"view1-password-123"}' "$U/api/auth/login" >/dev/null
check "dev1 signs in as a developer" jqe "$(curl -fsSk -b "$DJ" "$U/api/auth/me")" '.username=="dev1" and .role=="developer" and .projects==["portal.test.local"]'
check "wrong password for dev1 refused" test "$(code -H "Origin: $U" -H 'Content-Type: application/json' -d '{"username":"dev1","password":"nope-nope-nope"}' "$U/api/auth/login")" = 401
check "dev1 cannot list users (403)" test "$(code -b "$DJ" "$U/api/users")" = 403
check "dev1 cannot create users (403)" test "$(code -b "$DJ" -H "Origin: $U" -H 'Content-Type: application/json' -d '{"username":"x","role":"admin","password":"xxxxxxxxxxxxxx"}' "$U/api/users")" = 403
check "dev1 cannot change another site (403)" test "$(code -b "$DJ" -H "Origin: $U" -H 'Content-Type: application/json' -d '{"kind":"site_root","params":{"id":"shop.test.local","root":"/tmp"}}' "$U/api/plans/preview")" = 403
check "viewer can read the dashboard" test "$(code -b "$VJ" "$U/api/overview")" = 200
check "viewer cannot change anything (403)" test "$(code -b "$VJ" -H "Origin: $U" -H 'Content-Type: application/json' -d '{}' "$U/api/projects/portal.test.local/deploy")" = 403
check "viewer cannot open files (403)" test "$(code -b "$VJ" "$U/api/files?path=$PORTAL_DIR")" = 403

RT="$(post_json account/tokens '{"name":"ci read","scope":"read","expires_days":30}' | jq -r .secret)"
DT="$(post_json account/tokens '{"name":"ci deploy","scope":"deploy"}' | jq -r .secret)"
check "token secrets look like nc_…" bash -c "[[ '$RT' == nc_* && '$DT' == nc_* ]]"
check "read token: GET works" test "$(code -H "Authorization: Bearer $RT" "$U/api/overview")" = 200
check "read token: POST refused (403)" test "$(code -H "Authorization: Bearer $RT" -H 'Content-Type: application/json' -d '{}' "$U/api/projects/portal.test.local/deploy")" = 403
check "deploy token: system info refused (403)" test "$(code -H "Authorization: Bearer $DT" "$U/api/system")" = 403
check "deploy token: may start a deploy (portal has no Git → 409, not 403)" test "$(code -H "Authorization: Bearer $DT" -H 'Content-Type: application/json' -d '{}' "$U/api/projects/portal.test.local/deploy")" = 409
check "bad token → 401" test "$(code -H "Authorization: Bearer nc_nope" "$U/api/overview")" = 401
check "tokens are listed without secrets" jqe "$(api account/tokens)" '(length>=2) and all(.[]; has("secret")|not) and ([.[].last_used_at]|map(select(.!=null))|length)>=1'
TID="$(api account/tokens | jq -r '[.[] | select(.name=="ci read")][0].id')"
curl -fsSk -b "$JAR" -H "Origin: $U" -X DELETE "$U/api/account/tokens/$TID" >/dev/null
check "revoked token → 401" test "$(code -H "Authorization: Bearer $RT" "$U/api/overview")" = 401

SEC="$(post_json account/2fa/setup '{}' | jq -r .secret)"
check "2FA setup returns a base32 secret" bash -c "[[ '$SEC' =~ ^[A-Z2-7]{32}$ ]]"
check "wrong code does not turn 2FA on" test "$(post_code account/2fa/enable '{"code":"000000"}')" = 400
RC="$(post_json account/2fa/enable "{\"code\":\"$(python3 "$H/totp.py" "$SEC")\"}")"
check "2FA on; 10 recovery codes" jqe "$RC" '.enabled==true and (.recovery_codes|length)==10'
REC="$(jq -r '.recovery_codes[0]' <<<"$RC")"
check "password alone now asks for a code (401 totp_required)" bash -c "curl -sk -H 'Origin: $U' -H 'Content-Type: application/json' -d '{\"password\":\"$PASS\"}' '$U/api/auth/login' | jq -e '.error==\"totp_required\"'"
TJ="$(mktemp)"
sleep 1
C1="$(python3 "$H/totp.py" "$SEC" 1)"   # next step (allowed ±1) — not used yet
check "password + code signs in" curl -fsSk -c "$TJ" -H "Origin: $U" -H 'Content-Type: application/json' -d "{\"password\":\"$PASS\",\"code\":\"$C1\"}" "$U/api/auth/login"
R="$(code -H "Origin: $U" -H 'Content-Type: application/json' -d "{\"password\":\"$PASS\",\"code\":\"$C1\"}" "$U/api/auth/login")"; echo "    replay → $R"
check "the same code can't be used twice" test "$R" = 401
check "a recovery code signs in once" curl -fsSk -c "$TJ" -H "Origin: $U" -H 'Content-Type: application/json' -d "{\"password\":\"$PASS\",\"code\":\"$REC\"}" "$U/api/auth/login"
R="$(code -H "Origin: $U" -H 'Content-Type: application/json' -d "{\"password\":\"$PASS\",\"code\":\"$REC\"}" "$U/api/auth/login")"; echo "    recovery replay → $R"
check "…and not twice" test "$R" = 401
check "me reports 2FA on" jqe "$(api auth/me)" '.totp_enabled==true'
sudo_on "$PASS"
check "turn 2FA off again" jqe "$(post_json account/2fa/disable '{}')" '.enabled==false'
check "password alone works again" login "$PASS"
sudo_on "$PASS"

step "Cron jobs"
CJ="$(post_json cron '{"schedule":"*/5 * * * *","command":"echo hi-from-cron $((40+2)) 50%"}')"
CJID="$(jq -r .id <<<"$CJ")"
check "server cron job created (runs as root)" jqe "$CJ" '.user=="root" and .enabled==true'
check "bad schedule refused" test "$(post_code cron '{"schedule":"every day","command":"true"}')" = 400
check "/etc/cron.d/nexacloud-jobs has the job (% escaped)" in_box "grep -q 'cron-run $CJID' /etc/cron.d/nexacloud-jobs && grep -q '50\\\\%' /etc/cron.d/nexacloud-jobs"
st="$(wait_job2 "$(post_json "cron/$CJID/run" '{}' | jq -r .job_id)")"
check "run now → ok" test "$st" = ok
check "last output kept (hi-from-cron 42 50%)" jqe "$(api "cron/$CJID/log")" '(.log|test("hi-from-cron 42 50%")) and .last_exit==0'
check "list shows the last run" jqe "$(api cron)" "[.[] | select(.id==\"$CJID\")][0].last_exit==0"
PJ="$(post_json cron '{"project_id":"portal.test.local","schedule":"@hourly","command":"echo whoami=$(id -un) dir=$(pwd)"}')"
PJID="$(jq -r .id <<<"$PJ")"
PU="$(api projects/portal.test.local/overview | jq -r .permissions.web_user)"
PO="$(in_box "stat -c %U $PORTAL_DIR")"
check "project job runs as the project's PHP user ($PU) in its folder" jqe "$PJ" ".user==\"$PU\" and .cwd==\"$PORTAL_DIR\""
st="$(wait_job2 "$(post_json "cron/$PJID/run" '{}' | jq -r .job_id)")"
check "project job ran as $PU in $PORTAL_DIR" jqe "$(api "cron/$PJID/log")" ".log|test(\"whoami=$PU dir=$PORTAL_DIR\")"
check "dev1 sees only portal's job" bash -c "curl -fsSk -b '$DJ' '$U/api/cron' | jq -e 'length==1 and .[0].project_id==\"portal.test.local\"'"
check "dev1 cannot add a server job (403)" test "$(code -b "$DJ" -H "Origin: $U" -H 'Content-Type: application/json' -d '{"schedule":"* * * * *","command":"id"}' "$U/api/cron")" = 403
check "pause a job" jqe "$(curl -fsSk -b "$JAR" -H "Origin: $U" -H 'Content-Type: application/json' -X PUT -d '{"schedule":"*/5 * * * *","command":"echo hi-from-cron $((40+2)) 50%","enabled":false}' "$U/api/cron/$CJID")" '.enabled==false'
check "paused job is left out of cron" in_box "! grep -q 'cron-run $CJID' /etc/cron.d/nexacloud-jobs"
for id in "$CJID" "$PJID"; do curl -fsSk -b "$JAR" -H "Origin: $U" -X DELETE "$U/api/cron/$id" >/dev/null; done
check "deleted jobs are gone from cron" in_box "! grep -q '$CJID' /etc/cron.d/nexacloud-jobs"

step "File manager"
FM=/var/www/nc-fm-test
fpost() { post_json "files/$1" "$2"; }
check "roots: /var/www, /srv, /home writable" jqe "$(api files)" '[.roots[] | select(.writable)] | map(.path) | index("/var/www")!=null'
check "mkdir" jqe "$(fpost mkdir "{\"path\":\"$FM\"}")" '.ok'
WB="$(jq -nc --arg p "$FM/hello.txt" '{path:$p, content:"hello nexacloud\n"}')"
check "write a file" jqe "$(fpost write "$WB")" '.created==true'
check "read it back" jqe "$(api "files/read?path=$FM/hello.txt")" '.content=="hello nexacloud\n" and .writable==true'
check "listing shows it" jqe "$(api "files?path=$FM")" '[.entries[].name]==["hello.txt"]'
printf 'uploaded-bytes' > /tmp/nc-up.bin
check "upload (raw body)" bash -c "curl -fsSk -b '$JAR' -H 'Origin: $U' --data-binary @/tmp/nc-up.bin '$U/api/files/upload?dir=$FM&name=up.bin' | jq -e '.size==14'"
check "download returns the same bytes" bash -c "[ \"\$(curl -fsSk -b '$JAR' '$U/api/files/download?path=$FM/up.bin')\" = uploaded-bytes ]"
B="{\"path\":\"$FM/hello.txt\",\"mode\":\"640\"}"
check "chmod 640" jqe "$(fpost chmod "$B")" '.ok'
check "…mode is 640" in_box "[ \$(stat -c %a $FM/hello.txt) = 640 ]"
B="{\"path\":\"$FM/hello.txt\",\"mode\":\"777\"}"
check "chmod 777 refused" test "$(post_code files/chmod "$B")" = 400
B="{\"from\":\"$FM/up.bin\",\"to\":\"$FM/up2.bin\"}"
check "rename" jqe "$(fpost rename "$B")" '.ok'
B="{\"paths\":[\"$FM/hello.txt\",\"$FM/up2.bin\"],\"dest\":\"$FM/pack.zip\"}"
check "compress to zip" jqe "$(fpost compress "$B")" '.ok'
B="{\"path\":\"$FM/pack.zip\",\"dest\":\"$FM/out\"}"
check "extract into a folder" jqe "$(fpost extract "$B")" '.ok'
check "…extracted files are there" in_box "grep -q 'hello nexacloud' $FM/out/hello.txt && [ -f $FM/out/up2.bin ]"
check "path traversal refused (403)" test "$(code -b "$JAR" "$U/api/files/read?path=/var/www/../../etc/shadow")" = 403
in_box "ln -sfn /etc $FM/etc-link"
check "symlink out of the allowed folders refused (403)" test "$(code -b "$JAR" "$U/api/files/read?path=$FM/etc-link/shadow")" = 403
check "/etc/nginx is read only (write → 403)" test "$(post_code files/write '{"path":"/etc/nginx/x.conf","content":"x"}')" = 403
check "/var/www itself can't be deleted (403)" test "$(post_code files/delete '{"paths":["/var/www"]}')" = 403
check "dev1 can list their project folder" test "$(code -b "$DJ" "$U/api/files?path=$PORTAL_DIR")" = 200
check "dev1 cannot open another folder (403)" test "$(code -b "$DJ" "$U/api/files?path=$FM")" = 403
check "files written in a project folder keep the folder's owner" bash -c "curl -fsSk -b '$DJ' -H 'Origin: $U' -H 'Content-Type: application/json' -d '{\"path\":\"$PORTAL_DIR/nc-owner.txt\",\"content\":\"x\"}' '$U/api/files/write' >/dev/null && [ \"\$(docker exec $NAME stat -c %U $PORTAL_DIR/nc-owner.txt)\" = '$PO' ]"
B="{\"paths\":[\"$FM\",\"$PORTAL_DIR/nc-owner.txt\"]}"
check "delete" jqe "$(fpost delete "$B")" '.ok'
check "…gone" in_box "[ ! -e $FM ]"

step "Web terminal"
sudo_on "$PASS"
check "root terminal runs commands" python3 "$H/ws_term.py" "$U" "$JAR" "cols=100&rows=30" 'echo nc-term-$((6*7)) $(id -un)' 'nc-term-42 root'
check "project terminal runs as the project's user" python3 "$H/ws_term.py" "$U" "$JAR" "project=portal.test.local" 'echo nc-term-$((6*7)) $(id -un) $(pwd)' "nc-term-42 $PU $PORTAL_DIR"
check "foreign Origin refused" bash -c "python3 '$H/ws_term.py' '$U' '$JAR' 'cols=80' 'id' 'x' 'https://evil.example' | grep -q 403"
check "dev1 cannot open a root terminal" bash -c "python3 '$H/ws_term.py' '$U' '$DJ' 'cols=80' 'id' 'x' | grep -q 403"
check "terminal sessions are in the audit log" jqe "$(api audit)" '[.[] | select(.action=="terminal.open")] | length>=2'
check "delete users" bash -c "curl -fsSk -b '$JAR' -H 'Origin: $U' -X DELETE '$U/api/users/dev1' && curl -fsSk -b '$JAR' -H 'Origin: $U' -X DELETE '$U/api/users/view1'"
check "deleted user's session is gone (401)" test "$(code -b "$DJ" "$U/api/auth/me")" = 401
rm -f "$DJ" "$VJ" "$TJ"

# ------------------------------------------------------------------ batch B
step "Site security: blocked IPs, protected folders, error pages, PHP limits, own certificate"
sudo_on "$PASS"
app_code() { in_box "curl -s -o /dev/null -w '%{http_code}' ${2:-} -H 'Host: app.test.local' http://127.0.0.1$1"; }
wait_code() { local want="$1"; shift; for _ in $(seq 1 10); do [ "$(app_code "$@")" = "$want" ] && return 0; sleep 1; done; return 1; }
opts() { api sites/app.test.local/model | jq -c ".options | $1"; }
APP_ROOT="$(api sites/app.test.local/model | jq -r .root)"
in_box "mkdir -p $APP_ROOT/errors && echo 'custom-403-page' > $APP_ROOT/errors/403.html && chmod -R a+rX $APP_ROOT/errors"
P="$(jq -nc --argjson o "$(opts '.deny_ips=["127.0.0.1"] | .error_pages=[{"code":403,"path":"/errors/403.html"}]')" '{id:"app.test.local",options:$o}')"
check "block 127.0.0.1 on app.test.local with a custom 403 page → applied" test "$(plan site_options "$P")" = applied
check "…blocked visitor gets 403" wait_code 403 /
check "…with the custom error page" in_box "curl -s -H 'Host: app.test.local' http://127.0.0.1/ | grep -q custom-403-page"
check "…other sites are not affected" in_box "[ \$(curl -s -o /dev/null -w '%{http_code}' -H 'Host: shop.test.local' http://127.0.0.1/) != 403 ]"
P="$(jq -nc --argjson o "$(opts '.deny_ips=[] | .error_pages=[]')" '{id:"app.test.local",options:$o}')"
check "unblock → applied" test "$(plan site_options "$P")" = applied
check "…200 again" wait_code 200 /
check "bad IP refused" test "$(post_code plans/preview '{"kind":"site_options","params":{"id":"app.test.local","options":{"deny_ips":["1.2.3.4; allow all"]}}}')" = 409

B='{"users":[{"name":"client","password":"client-pass-1"}]}'
check "add a folder user (hashed)" bash -c "curl -fsSk -b '$JAR' -H 'Origin: $U' -H 'Content-Type: application/json' -X PUT -d '$B' '$U/api/sites/app.test.local/auth-users' | jq -e '.users==[\"client\"]' && docker exec $NAME grep -q '^client:\\\$6\\\$' /etc/nginx/nexacloud-auth/app.test.local"
check "password file is not world-readable" in_box "[ \$(stat -c %a /etc/nginx/nexacloud-auth/app.test.local) = 640 ]"
P="$(jq -nc --argjson o "$(opts '.protected=[{"path":"/secret","realm":"Staff only"}]')" '{id:"app.test.local",options:$o}')"
check "protect /secret → applied" test "$(plan site_options "$P")" = applied
check "…/secret asks for a password (401)" wait_code 401 /secret/
check "…wrong password → 401" test "$(app_code /secret/ '-u client:nope')" = 401
check "…right password → served by the app" test "$(app_code /secret/ '-u client:client-pass-1')" = 200
check "…the rest of the site stays open" test "$(app_code / '')" = 200
P="$(jq -nc --argjson o "$(opts '.protected=[{"path":"/","realm":"Staging"}]')" '{id:"app.test.local",options:$o}')"
check "protect the whole site → applied (probe accepts 401)" test "$(plan site_options "$P")" = applied
check "…home page needs the password" wait_code 401 /
check "…Let's Encrypt challenges stay reachable" bash -c "docker exec $NAME sh -c 'mkdir -p /var/www/_acme/.well-known/acme-challenge && echo tok > /var/www/_acme/.well-known/acme-challenge/t1' ; [ \"\$(docker exec $NAME curl -s -o /dev/null -w '%{http_code}' -H 'Host: app.test.local' http://127.0.0.1/.well-known/acme-challenge/t1)\" != 401 ]"
P="$(jq -nc --argjson o "$(opts '.protected=[]')" '{id:"app.test.local",options:$o}')"
check "remove protection → applied" test "$(plan site_options "$P")" = applied
check "…open again" wait_code 200 /

P='{"id":"app.test.local","pool":{"pm":"dynamic","max_children":7,"memory_limit":"384M","upload_max_filesize":"96M","max_execution_time":90}}'
check "tune PHP pool → applied" test "$(plan site_pool "$P")" = applied
check "…pool file has the limits" in_box "f=/etc/php/8.3/fpm/pool.d/nc_app_test_local.conf; grep -q 'pm = dynamic' \$f && grep -q 'pm.max_children = 7' \$f && grep -q 'memory_limit\\] = 384M' \$f && grep -q 'post_max_size\\] = 96M' \$f"
check "…site still served" wait_code 200 /
check "bad pool settings refused" test "$(post_code plans/preview '{"kind":"site_pool","params":{"id":"app.test.local","pool":{"pm":"turbo","max_children":7}}}')" = 409
check "shared-pool site can't be tuned (needs its own user)" test "$(post_code plans/preview '{"kind":"site_pool","params":{"id":"down.test.local","pool":{"pm":"ondemand","max_children":5}}}')" = 409

in_box "cd /tmp && openssl req -x509 -newkey rsa:2048 -nodes -keyout own.key -out own.crt -days 30 -subj '/CN=app.test.local' 2>/dev/null && openssl genrsa -out other.key 2048 2>/dev/null"
P="$(jq -nc --arg c "$(in_box 'cat /tmp/own.crt')" --arg k "$(in_box 'cat /tmp/other.key')" '{id:"app.test.local",cert:$c,key:$k}')"
st="$(plan site_ssl_custom "$P" 2>/dev/null)"
check "a key that doesn't match the certificate is refused (reverted)" bash -c "[ '$st' != applied ] && [ \"\$(docker exec $NAME curl -s -o /dev/null -w '%{http_code}' -H 'Host: app.test.local' http://127.0.0.1/)\" = 200 ]"
P="$(jq -nc --arg c "$(in_box 'cat /tmp/own.crt')" --arg k "$(in_box 'cat /tmp/own.key')" '{id:"app.test.local",cert:$c,key:$k}')"
check "install own certificate → applied" test "$(plan site_ssl_custom "$P")" = applied
check "…HTTPS uses it (CN=app.test.local)" in_box "echo | openssl s_client -connect 127.0.0.1:443 -servername app.test.local 2>/dev/null | openssl x509 -noout -subject | grep -q 'CN *= *app.test.local'"
check "…HTTP redirects to HTTPS" test "$(app_code / '')" = 301
check "…key file is root-only" in_box "[ \$(stat -c %a /etc/nexacloud/ssl/app.test.local/privkey.pem) = 600 ]"
check "…the stored change never shows the key" bash -c "! curl -fsSk -b '$JAR' '$U/api/changes?limit=5' | grep -q \"\$(docker exec $NAME sed -n 2p /tmp/own.key)\""

step "Block IPs on every site; import .sql; SFTP accounts"
check "block list starts empty" jqe "$(api blocklist)" '.ips==[]'
check "block 127.0.0.1 everywhere → applied" test "$(plan ip_block '{"ips":["127.0.0.1"]}')" = applied
check "…shop.test.local (HTTPS) now 403 too" in_box "[ \$(curl -sk -o /dev/null -w '%{http_code}' --resolve shop.test.local:443:127.0.0.1 https://shop.test.local/) = 403 ]"
check "…panel still reachable" curl -fsk "$U/api/health"
check "blocking everyone is refused" test "$(post_code plans/preview '{"kind":"ip_block","params":{"ips":["0.0.0.0/0"]}}')" = 409
check "clear the block list → applied" test "$(plan ip_block '{"ips":[]}')" = applied
check "…shop.test.local answers again" in_box "[ \$(curl -sk -o /dev/null -w '%{http_code}' --resolve shop.test.local:443:127.0.0.1 https://shop.test.local/) = 200 ]"

printf 'CREATE TABLE imported_t (id INT);\nINSERT INTO imported_t VALUES (42),(43);\n' | gzip > /tmp/nc-import.sql.gz
jid="$(curl -fsSk -b "$JAR" -H "Origin: $U" --data-binary @/tmp/nc-import.sql.gz "$U/api/dbm/mysql/shop_prod/import?name=dump.sql.gz" | jq -r .job_id)"
st="$(wait_job2 "$jid")"; [ "$st" = ok ] || api "jobs/$jid" | jq -r .log | tail -20
check "import dump.sql.gz into shop_prod → ok" test "$st" = ok
check "…rows are there" in_box "[ \$(mysql -N -e 'SELECT COUNT(*) FROM shop_prod.imported_t') = 2 ]"
check "…a backup was taken first" jqe "$(api "jobs/$jid")" '.log|test("Backup saved")'
check "…the upload is cleaned up" in_box "[ -z \"\$(ls -A /var/lib/nexacloud/imports 2>/dev/null)\" ]"
check "import of a .txt refused" test "$(code -b "$JAR" -H "Origin: $U" --data-binary 'x' "$U/api/dbm/mysql/shop_prod/import?name=x.txt")" = 400
check "import into an unknown database refused" test "$(code -b "$JAR" -H "Origin: $U" --data-binary @/tmp/nc-import.sql.gz "$U/api/dbm/mysql/nope_db/import?name=a.sql.gz")" = 404
in_box "mysql -e 'DROP TABLE shop_prod.imported_t'"

in_box "rm -f /tmp/sftpkey /tmp/sftpkey.pub && ssh-keygen -q -t ed25519 -N '' -f /tmp/sftpkey"
APP_DIR="$(api projects/app.test.local | jq -r .dir)"
B="$(jq -nc --arg k "$(in_box 'cat /tmp/sftpkey.pub')" '{username:"appftp",site_id:"app.test.local",password:"sftp-password-123",ssh_key:$k}')"
check "create SFTP account for app.test.local" jqe "$(post_json sftp "$B")" '.ok==true'
check "…listed and mounted" jqe "$(api sftp)" '[.[] | select(.username=="appftp")][0] | .mounted==true and .has_key==true'
check "…no shell login" in_box "[ \$(getent passwd appftp | cut -d: -f7) = /usr/sbin/nologin ]"
check "sshd config still valid" in_box "sshd -t"
in_box "echo sftp-hello > /tmp/up.txt"
SF="$(in_box "printf 'put /tmp/up.txt\nls -1 /\npwd\n' | sftp -q -b - -i /tmp/sftpkey -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null appftp@127.0.0.1 2>&1")"; echo "    sftp: $(echo "$SF" | tr '\n' ' ' | cut -c1-160)"
check "…upload lands in the site folder, owned by the site user" in_box "[ -f $APP_DIR/up.txt ] && [ \$(stat -c %U $APP_DIR/up.txt) = \$(stat -c %U $APP_DIR) ]"
printf '%s\n' "$SF" > /tmp/nc-sftp.out
check "…jailed: / only shows the site folder" bash -c "grep -qx '/site' /tmp/nc-sftp.out && ! grep -q '^/etc' /tmp/nc-sftp.out"
check "…shell commands refused" bash -c "! docker exec $NAME ssh -i /tmp/sftpkey -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes appftp@127.0.0.1 id 2>/dev/null | grep -q uid="
check "duplicate / system usernames refused" test "$(post_code sftp '{"username":"root","site_id":"app.test.local","password":"sftp-password-123"}')" = 400
check "delete SFTP account" jqe "$(curl -fsSk -b "$JAR" -H "Origin: $U" -X DELETE "$U/api/sftp/appftp")" '.ok==true'
check "…the site's PHP workers kept running" in_box "pgrep -u nc_app_test_local php-fpm >/dev/null"
check "…user, mount and fstab line gone; files kept" in_box "! getent passwd appftp && ! grep -q nexacloud-sftp:appftp /etc/fstab && ! mountpoint -q /srv/sftp/appftp/site 2>/dev/null && [ -f $APP_DIR/up.txt ] && rm -f $APP_DIR/up.txt"

# ------------------------------------------------------------------ batch C
step "Server tools: timezone, updates, SSH keys, reboot guard"
in_box "printf '[Service]\nEnvironment=NEXACLOUD_CLOUDFLARE_API=http://127.0.0.1:8098/cf\nEnvironment=NEXACLOUD_HOSTINGER_API=http://127.0.0.1:8098/hs\nEnvironment=NEXACLOUD_WP_URL=file:///opt/wordpress-latest.tar.gz\nEnvironment=NEXACLOUD_CLAMAV_DB=/opt/clamdb\n' > /etc/systemd/system/nexacloud.service.d/batchc.conf && systemctl daemon-reload && systemctl restart nexacloud"
wait_health; login "$PASS"; sudo_on "$PASS"
check "server info has a timezone" jqe "$(api server)" '(.timezone|type)=="string" and (.uptime_secs|type)=="number"'
check "timezone list includes Europe/Amsterdam" jqe "$(api server/timezones)" 'index("Europe/Amsterdam")!=null'
check "set timezone Europe/Amsterdam" jqe "$(curl -fsSk -b "$JAR" -H "Origin: $U" -H 'Content-Type: application/json' -X PUT -d '{"timezone":"Europe/Amsterdam"}' "$U/api/server/timezone")" '.timezone=="Europe/Amsterdam"'
check "…the server uses it" in_box "readlink /etc/localtime | grep -q Europe/Amsterdam"
check "unknown timezone refused" test "$(code -b "$JAR" -H "Origin: $U" -H 'Content-Type: application/json' -X PUT -d '{"timezone":"Mars/Base"}' "$U/api/server/timezone")" = 400
curl -fsSk -b "$JAR" -H "Origin: $U" -H 'Content-Type: application/json' -X PUT -d '{"timezone":"UTC"}' "$U/api/server/timezone" >/dev/null
st="$(wait_job2 "$(post_json server/updates/check '{}' | jq -r .job_id)")"
check "check for updates (apt-get update) → ok" test "$st" = ok
check "updates list" jqe "$(api server/updates)" '(.packages|type)=="array" and (.security|type)=="number"'
check "reboot needs the server name typed" test "$(post_code server/reboot '{"confirm":"nope"}')" = 400
in_box "rm -f /tmp/rk1 /tmp/rk1.pub /tmp/rk2 /tmp/rk2.pub && ssh-keygen -q -t ed25519 -N '' -C first@test -f /tmp/rk1 && ssh-keygen -q -t ed25519 -N '' -C second@test -f /tmp/rk2 && rm -f /root/.ssh/authorized_keys"
K1="$(jq -nc --arg k "$(in_box 'cat /tmp/rk1.pub')" '{key:$k}')"; K2="$(jq -nc --arg k "$(in_box 'cat /tmp/rk2.pub')" '{key:$k}')"
FP1="$(post_json server/ssh-keys "$K1" | jq -r .fingerprint)"
check "add root SSH key" bash -c "[[ '$FP1' == SHA256:* ]]"
check "…listed with its comment" jqe "$(api server/ssh-keys)" '[.keys[] | select(.comment=="first@test")] | length==1'
check "duplicate key refused" test "$(post_code server/ssh-keys "$K1")" = 400
check "garbage key refused" test "$(post_code server/ssh-keys '{"key":"ssh-ed25519 notakey"}')" = 400
check "removing the last key is refused" test "$(code -b "$JAR" -H "Origin: $U" -X DELETE "$U/api/server/ssh-keys/$(jq -rn --arg f "$FP1" '$f|@uri')")" = 400
post_json server/ssh-keys "$K2" >/dev/null
check "with two keys, one can be removed" jqe "$(curl -fsSk -b "$JAR" -H "Origin: $U" -X DELETE "$U/api/server/ssh-keys/$(jq -rn --arg f "$FP1" '$f|@uri')")" '.ok==true'
check "…file is 600 and has only the second key" in_box "[ \$(stat -c %a /root/.ssh/authorized_keys) = 600 ] && grep -q second@test /root/.ssh/authorized_keys && ! grep -q first@test /root/.ssh/authorized_keys"

step "WordPress in one click"
check "username 'admin' refused" test "$(post_code apps/wordpress '{"domain":"wp.test.local","php_version":"8.3","title":"T","admin_user":"admin","admin_email":"a@example.com","admin_password":"wp-admin-pass-123"}')" = 400
WJ="$(post_json apps/wordpress '{"domain":"wp.test.local","php_version":"8.3","title":"NC Blog","admin_user":"sara","admin_email":"sara@example.com","admin_password":"wp-admin-pass-123"}' | jq -r .job_id)"
st="$(wait_job2 "$WJ")"; [ "$st" = ok ] || api "jobs/$WJ" | jq -r .log | tail -30
check "WordPress installer → ok" test "$st" = ok
check "…home page is the WordPress blog" in_box "curl -s -H 'Host: wp.test.local' http://127.0.0.1/ | grep -q 'NC Blog'"
check "…admin account sara exists" in_box "mysql -N -e 'SELECT user_login FROM wp_test_local.wp_users' | grep -qx sara"
check "…wp-config.php readable by the site user only" in_box "[ \"\$(stat -c '%a %U' /var/www/wp.test.local/wp-config.php)\" = '640 nc_wp_test_local' ]"
check "…files owned by the site user, PHP runs as it" in_box "[ \$(stat -c %U /var/www/wp.test.local/wp-settings.php) = nc_wp_test_local ] && [ -f /etc/php/8.3/fpm/pool.d/nc_wp_test_local.conf ]"
check "…sign-in page answers" in_box "[ \$(curl -s -o /dev/null -w '%{http_code}' -H 'Host: wp.test.local' http://127.0.0.1/wp-login.php) = 200 ]"
check "…listed as a project" jqe "$(api projects)" '[.[] | select(.id=="wp.test.local")] | length==1'
check "same domain twice refused" test "$(post_code apps/wordpress '{"domain":"wp.test.local","php_version":"8.3","title":"T","admin_user":"bob","admin_email":"a@example.com","admin_password":"wp-admin-pass-123"}')" = 400

step "Node.js app"
in_box 'set -e
rm -rf /srv/git/nodeapp.git /tmp/nodesrc && git init -q --bare /srv/git/nodeapp.git && mkdir /tmp/nodesrc && cd /tmp/nodesrc
printf "{\"name\":\"nodeapp\",\"version\":\"1.0.0\",\"scripts\":{\"start\":\"node server.js\"}}\n" > package.json
printf "require(\"http\").createServer((q, r) => r.end(\"node v1 on \" + process.env.PORT + \" \" + (process.env.GREETING||\"\"))).listen(process.env.PORT)\n" > server.js
git init -q -b main && git config user.email dev@example.com && git config user.name Dev && git add -A && git commit -qm v1 && git remote add origin /srv/git/nodeapp.git && git push -q origin main && chmod -R a+rX /srv/git/nodeapp.git'
NJ="$(post_json apps/node '{"domain":"node.test.local","repo":"/srv/git/nodeapp.git","branch":"main","env":{"GREETING":"hello"}}')"
NP="$(jq -r .port <<<"$NJ")"
st="$(wait_job2 "$(jq -r .job_id <<<"$NJ")")"; [ "$st" = ok ] || api "jobs/$(jq -r .job_id <<<"$NJ")" | jq -r .log | tail -30
check "Node.js wizard → ok (port $NP)" test "$st" = ok
check "…first deploy ok" bash -c "for i in \$(seq 1 300); do s=\$(curl -fsSk -b '$JAR' '$U/api/projects/node.test.local/deploys' | jq -r '.[0].status // empty'); case \$s in ok) exit 0;; failed|rolled_back) exit 1;; esac; sleep 1; done; exit 1"
check "…app answers through nginx with PORT and .env" bash -c "for i in \$(seq 1 20); do docker exec $NAME curl -s -H 'Host: node.test.local' http://127.0.0.1/ | grep -qx 'node v1 on $NP hello' && exit 0; sleep 1; done; exit 1"
check "…runs as its own user under systemd" in_box "systemctl is-active --quiet nexacloud-node-node-test-local && [ \$(ps -o user= -p \$(systemctl show -p MainPID --value nexacloud-node-node-test-local)) = nc_node_test_local ]"
check "…status endpoint" jqe "$(api projects/node.test.local/node)" ".active==\"active\" and .port==$NP"
check "…listed as a Node project" jqe "$(api workspace)" '[.[] | select(.id=="node.test.local")][0].project.kind=="node"'
in_box "cd /tmp/nodesrc && sed -i 's/node v1/node v2/' server.js && git commit -qam v2 && git push -q origin main && chmod -R a+rX /srv/git/nodeapp.git"
st="$(wait_job2 "$(post_json projects/node.test.local/deploy '{}' | jq -r .job_id)")"
check "deploy v2 → restarted and serving v2" bash -c "[ '$st' = ok ] && for i in \$(seq 1 20); do docker exec $NAME curl -s -H 'Host: node.test.local' http://127.0.0.1/ | grep -q 'node v2' && exit 0; sleep 1; done; exit 1"
check "restart button" jqe "$(post_json projects/node.test.local/node/restart '{}')" '.ok==true'

step "Malware scan, visitor stats"
st="$(wait_job2 "$(post_json malware/install '{}' | jq -r .job_id)")"
check "ClamAV ready (test signatures)" bash -c "[ '$st' = ok ] && curl -fsSk -b '$JAR' '$U/api/malware' | jq -e '.installed==true'"
in_box "mkdir -p /var/www/wp.test.local/wp-content/uploads && echo WDVPIVAlQEFQWzRcUFpYNTQoUF4pN0NDKTd9JEVJQ0FSLVNUQU5EQVJELUFOVElWSVJVUy1URVNULUZJTEUhJEgrSCo= | base64 -d > /var/www/wp.test.local/wp-content/uploads/evil.php"
check "test file is the 68-byte EICAR string" in_box "[ \$(md5sum < /var/www/wp.test.local/wp-content/uploads/evil.php | cut -c1-32) = 44d88612fea8a8f36de82e1278abb02f ]"
st="$(wait_job2 "$(post_json malware/scan '{"path":"/var/www/wp.test.local"}' | jq -r .job_id)")"
check "scan wp.test.local → ok" test "$st" = ok
check "…found the test file" jqe "$(api malware)" '.last.found==[{"file":"/var/www/wp.test.local/wp-content/uploads/evil.php","signature":"Eicar-Test-Signature.UNOFFICIAL"}] or (.last.found|length==1 and (.[0].file|endswith("evil.php")))'
check "quarantine it" jqe "$(post_json malware/quarantine '{"file":"/var/www/wp.test.local/wp-content/uploads/evil.php"}')" '.ok==true'
check "…moved out of the site, root-only" in_box "[ ! -e /var/www/wp.test.local/wp-content/uploads/evil.php ] && [ \$(ls /var/lib/nexacloud/quarantine | wc -l) = 1 ]"
check "scanning /etc is refused" test "$(post_code malware/scan '{"path":"/etc"}')" = 400

check "stats are off until turned on" jqe "$(api sites/wp.test.local/stats)" '.enabled==false'
check "turn on visitor stats → applied" test "$(plan stats_enable '{}')" = applied
sleep 2
in_box "for i in 1 2 3; do curl -s -o /dev/null -A 'Mozilla/5.0 Chrome/120' -e https://news.example/x -H 'Host: wp.test.local' 'http://127.0.0.1/?p=1'; done; curl -s -o /dev/null -A 'Googlebot/2.1' -H 'Host: wp.test.local' http://127.0.0.1/; curl -s -o /dev/null -A 'Mozilla/5.0 Chrome/120' -H 'Host: node.test.local' http://127.0.0.1/"
check "stats for wp.test.local: 3 page requests, 1 bot, referrer" bash -c "for i in \$(seq 1 10); do curl -fsSk -b '$JAR' '$U/api/sites/wp.test.local/stats?days=1' | jq -e '.totals.human_requests>=3 and .totals.bot_requests>=1 and .totals.visitors>=1 and (.top_referrers[0].name==\"news.example\") and (.top_pages[0].name==\"/\")' >/dev/null && exit 0; sleep 1; done; exit 1"
check "…other sites' hits not counted" jqe "$(api 'sites/node.test.local/stats?days=1')" '.totals.human_requests>=1 and (.top_referrers|length)==0'

step "DNS zone editor (Cloudflare + Hostinger APIs, faked)"
check "bad token refused (checked against the provider)" test "$(post_code dns/providers '{"kind":"cloudflare","name":"cf","token":"bad-token-xxxxxxxxxxxxxxxxx"}')" = 400
CFP="$(post_json dns/providers '{"kind":"cloudflare","name":"My Cloudflare","token":"good-dns-token-0123456789abcdef"}')"
CFID="$(jq -r .id <<<"$CFP")"
check "connect Cloudflare; token never sent back" bash -c "[ -n '$CFID' ] && ! curl -fsSk -b '$JAR' '$U/api/dns/providers' | grep -q good-dns-token"
B='{"type":"A","name":"www","content":"203.0.113.10","ttl":1,"proxied":true}'
check "add A record www" jqe "$(post_json "dns/$CFID/zones/example.test/records" "$B")" '.ok==true'
R="$(api "dns/$CFID/zones/example.test/records")"
RID="$(jq -r '.records[] | select(.name=="www") | .id' <<<"$R")"
check "…listed as www (relative name), proxied" jqe "$R" '[.records[] | select(.name=="www" and .content=="203.0.113.10" and .proxied==true)] | length==1'
check "invalid A record refused" test "$(post_code "dns/$CFID/zones/example.test/records" '{"type":"A","name":"x","content":"nope"}')" = 400
B='{"type":"A","name":"www","content":"203.0.113.11","ttl":300}'
check "edit it" bash -c "curl -fsSk -b '$JAR' -H 'Origin: $U' -H 'Content-Type: application/json' -X PUT -d '$B' '$U/api/dns/$CFID/zones/example.test/records/$RID' | jq -e .ok >/dev/null && curl -fsSk -b '$JAR' '$U/api/dns/$CFID/zones/example.test/records' | jq -e '[.records[] | select(.content==\"203.0.113.11\")] | length==1'"
B='{"provider":"google","dkim_selector":"google","dkim_value":"v=DKIM1; k=rsa; p=MIIBtest","dmarc_policy":"quarantine","dmarc_email":"dmarc@example.test"}'
check "email wizard (Google) → MX, SPF, DKIM, DMARC" bash -c "curl -fsSk -b '$JAR' -H 'Origin: $U' -H 'Content-Type: application/json' -d '$B' '$U/api/dns/$CFID/zones/example.test/email' | jq -e .ok >/dev/null && curl -fsSk -b '$JAR' '$U/api/dns/$CFID/zones/example.test/records' | jq -e '([.records[] | select(.type==\"MX\" and .content==\"smtp.google.com\")] | length==1) and ([.records[] | select(.name==\"_dmarc\")] | length==1) and ([.records[] | select(.name==\"google._domainkey\")] | length==1) and ([.records[] | select(.content|startswith(\"v=spf1 include:_spf.google.com\"))] | length==1)'"
check "running it again replaces instead of duplicating" bash -c "curl -fsSk -b '$JAR' -H 'Origin: $U' -H 'Content-Type: application/json' -d '$B' '$U/api/dns/$CFID/zones/example.test/email' >/dev/null && curl -fsSk -b '$JAR' '$U/api/dns/$CFID/zones/example.test/records' | jq -e '[.records[] | select(.type==\"MX\")] | length==1'"
check "delete the www record" jqe "$(curl -fsSk -b "$JAR" -H "Origin: $U" -X DELETE "$U/api/dns/$CFID/zones/example.test/records/$RID")" '.ok==true'
check "unknown zone → clear error" test "$(code -b "$JAR" "$U/api/dns/$CFID/zones/other.test/records")" = 404

HSID="$(post_json dns/providers '{"kind":"hostinger","name":"Hostinger","token":"good-dns-token-0123456789abcdef"}' | jq -r .id)"
check "connect Hostinger" test -n "$HSID"
post_json "dns/$HSID/zones/example.test/records" '{"type":"A","name":"@","content":"203.0.113.20","ttl":3600}' >/dev/null
post_json "dns/$HSID/zones/example.test/records" '{"type":"A","name":"@","content":"203.0.113.21","ttl":3600}' >/dev/null
check "Hostinger: two A records in one set" jqe "$(api "dns/$HSID/zones/example.test/records")" '[.records[] | select(.type=="A" and .name=="@")] | length==2'
HRID="$(api "dns/$HSID/zones/example.test/records" | jq -r '.records[] | select(.content=="203.0.113.20") | .id')"
check "Hostinger: delete one keeps the other" bash -c "curl -fsSk -b '$JAR' -H 'Origin: $U' -X DELETE '$U/api/dns/$HSID/zones/example.test/records/$(jq -rn --arg i "$HRID" '$i|@uri')' >/dev/null && curl -fsSk -b '$JAR' '$U/api/dns/$HSID/zones/example.test/records' | jq -e '[.records[] | select(.type==\"A\")] | map(.content)==[\"203.0.113.21\"]'"
check "Hostinger: MX via email wizard (priority kept)" bash -c "curl -fsSk -b '$JAR' -H 'Origin: $U' -H 'Content-Type: application/json' -d '{\"provider\":\"zoho\"}' '$U/api/dns/$HSID/zones/example.test/email' >/dev/null && curl -fsSk -b '$JAR' '$U/api/dns/$HSID/zones/example.test/records' | jq -e '[.records[] | select(.type==\"MX\")] | map(\"\(.priority) \(.content)\") == [\"10 mx.zoho.com\",\"20 mx2.zoho.com\",\"50 mx3.zoho.com\"]'"
check "wildcard SSL needs Cloudflare" test "$(post_code "dns/$HSID/wildcard" '{"zone":"example.test","email":"a@example.test"}')" = 400
check "disconnect a DNS account" jqe "$(curl -fsSk -b "$JAR" -H "Origin: $U" -X DELETE "$U/api/dns/providers/$HSID")" '.ok==true'

# ------------------------------------------------------------------ octane
step "Laravel Octane runtime (FrankenPHP worker mode)"
sudo_on "$PASS"
in_box 'set -e
rm -rf /srv/git/oct.git /tmp/octsrc && git init -q --bare /srv/git/oct.git
cp -a /opt/laravel-octane /tmp/octsrc && cd /tmp/octsrc && rm -rf .git .env database/database.sqlite
sed -i "/^\/vendor/d" .gitignore
cat >> routes/web.php <<"PHP"

Route::get("/version", fn () => "oct v1 " . (isset($_SERVER["LARAVEL_OCTANE"]) ? "octane" : "fpm"));
PHP
git init -q -b main && git config user.email dev@example.com && git config user.name Dev
git add -A && git commit -qm "oct v1" && git remote add origin /srv/git/oct.git && git push -q origin main
chmod -R a+rX /srv/git/oct.git'
OJ="$(post_json apps '{"kind":"laravel","domain":"octane.test.local","php_version":"8.3","repo":"/srv/git/oct.git","branch":"main","database":"none","isolate":true,"releases":true,"worker":false,"scheduler":false}' | jq -r .job_id)"
st="$(wait_job2 "$OJ")"; [ "$st" = ok ] || api "jobs/$OJ" | jq -r .log | tail -20
check "Laravel app for Octane → created" test "$st" = ok
check "…first deploy ok" bash -c "for i in \$(seq 1 600); do s=\$(curl -fsSk -b '$JAR' '$U/api/projects/octane.test.local/deploys' | jq -r '.[0].status // empty'); case \$s in ok) exit 0;; failed|rolled_back) exit 1;; esac; sleep 1; done; exit 1"
check "…served by PHP-FPM" bash -c "[ \"\$(docker exec $NAME curl -s -H 'Host: octane.test.local' http://127.0.0.1/version)\" = 'oct v1 fpm' ]"
check "Octane refused on a non-Laravel site" test "$(post_code plans/preview '{"kind":"site_runtime","params":{"id":"app.test.local","runtime":"octane"}}')" = 409
st="$(plan site_runtime '{"id":"octane.test.local","runtime":"octane"}')"
check "switch to Laravel Octane → applied" test "$st" = applied
check "…served by Octane workers" bash -c "for i in \$(seq 1 20); do [ \"\$(docker exec $NAME curl -s -H 'Host: octane.test.local' http://127.0.0.1/version)\" = 'oct v1 octane' ] && exit 0; sleep 1; done; exit 1"
check "…as the site user, under systemd" in_box "systemctl is-active --quiet nexacloud-frankenphp-octane-test-local && [ \$(ps -o user= -p \$(systemctl show -p MainPID --value nexacloud-frankenphp-octane-test-local)) = nc_octane_test_local ]"
check "…static files still served by nginx" in_box "[ \$(curl -s -o /dev/null -w '%{http_code}' -H 'Host: octane.test.local' http://127.0.0.1/robots.txt) = 200 ]"
in_box "cd /tmp/octsrc && sed -i 's/oct v1/oct v2/' routes/web.php && git commit -qam v2 && git push -q origin main && chmod -R a+rX /srv/git/oct.git"
st="$(wait_job2 "$(post_json projects/octane.test.local/deploy '{}' | jq -r .job_id)")"
check "deploy v2 → workers restarted, v2 live" bash -c "[ '$st' = ok ] && for i in \$(seq 1 20); do [ \"\$(docker exec $NAME curl -s -H 'Host: octane.test.local' http://127.0.0.1/version)\" = 'oct v2 octane' ] && exit 0; sleep 1; done; exit 1"
st="$(plan site_runtime '{"id":"octane.test.local","runtime":"fpm"}')"
check "switch back to PHP-FPM → applied" test "$st" = applied
check "…served by PHP-FPM again, Octane stopped" bash -c "for i in \$(seq 1 10); do [ \"\$(docker exec $NAME curl -s -H 'Host: octane.test.local' http://127.0.0.1/version)\" = 'oct v2 fpm' ] && ! docker exec $NAME systemctl is-active --quiet nexacloud-frankenphp-octane-test-local && exit 0; sleep 1; done; exit 1"

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
  NEWPASS="$(in_box 'nexacloud reset-password 2>/dev/null' | sed -n 's/^NEXACLOUD_ADMIN_PASSWORD=//p')"
  for _ in $(seq 1 30); do curl -fsk "$U/api/health" >/dev/null 2>&1 && break; sleep 1; done
  check "agent healthy after the last reset" curl -fsk "$U/api/health"
  printf '\n\033[1mAll %d checks passed.\033[0m Container kept running:\n  %s   password: %s\n' "$PASSES" "$U" "$NEWPASS"
  exit 0
fi
in_box "bash -s -- --uninstall" < scripts/install.sh >/dev/null
check "service removed" bash -c "! docker exec $NAME systemctl is-active --quiet nexacloud"
check "/opt/nexacloud removed" bash -c "! docker exec $NAME test -e /opt/nexacloud"
check "backups kept after uninstall" in_box "test -d /var/backups/nexacloud"
check "state + snapshots kept after uninstall" in_box "test -f /var/lib/nexacloud/state.db"
check "sites untouched (nginx still serves shop)" bash -c "docker exec $NAME curl -fsk -H 'Host: shop.test.local' https://127.0.0.1/ | grep -q 'shop ok'"

printf '\n\033[1;32mAll %d checks passed.\033[0m\n' "$PASSES"
