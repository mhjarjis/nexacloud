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
