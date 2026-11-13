#!/usr/bin/env bash
# NexaCloud installer — one command, safe to re-run (upgrades in place).
#
#   curl -fsSL https://raw.githubusercontent.com/mhjarjis/nexacloud/main/scripts/install.sh | sudo bash
#
# Options (pass after `bash -s --` when piping):
#   --version vX.Y.Z   install a specific release instead of the latest
#   --uninstall        remove the agent (keeps /etc/nexacloud and backups)
#   --purge            with --uninstall: also delete /etc/nexacloud
#
# Environment:
#   NEXACLOUD_TARBALL=/path/file.tar.gz   install from a local tarball (CI / offline)
#   NEXACLOUD_REPO=owner/repo             download releases from another repo
#
# What it touches (and nothing else): /opt/nexacloud, /etc/nexacloud,
# /var/backups/nexacloud, /usr/local/bin/nexacloud, /etc/systemd/system/nexacloud.service

set -euo pipefail

REPO="${NEXACLOUD_REPO:-mhjarjis/nexacloud}"
VERSION="latest"
TARBALL="${NEXACLOUD_TARBALL:-}"
ASSET="nexacloud-x86_64-linux.tar.gz"
PREFIX="/opt/nexacloud"
CONF_DIR="/etc/nexacloud"
CONF="$CONF_DIR/config.toml"
UNIT="/etc/systemd/system/nexacloud.service"
UNINSTALL=0
PURGE=0

if [ -t 1 ]; then
  c_green=$'\033[1;32m'; c_yellow=$'\033[1;33m'; c_red=$'\033[1;31m'; c_bold=$'\033[1m'; c_off=$'\033[0m'
else
  c_green=''; c_yellow=''; c_red=''; c_bold=''; c_off=''
fi
say()  { printf '%s==>%s %s\n' "$c_green" "$c_off" "$*"; }
warn() { printf '%s!!%s  %s\n' "$c_yellow" "$c_off" "$*" >&2; }
die()  { printf '%sxx%s  %s\n' "$c_red" "$c_off" "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="${2:?--version needs a value}"; shift 2 ;;
    --uninstall) UNINSTALL=1; shift ;;
    --purge) PURGE=1; shift ;;
    -h|--help) sed -n '2,17p' "$0" 2>/dev/null || true; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
done

[ "$(id -u)" -eq 0 ] || die "please run as root (e.g. curl … | sudo bash)"
command -v systemctl >/dev/null && [ -d /run/systemd/system ] || die "systemd is required"

# ---------------------------------------------------------------- uninstall
if [ "$UNINSTALL" -eq 1 ]; then
  say "Stopping and removing NexaCloud"
  systemctl disable --now nexacloud 2>/dev/null || true
  rm -f "$UNIT" /usr/local/bin/nexacloud
  systemctl daemon-reload
  rm -rf "$PREFIX"
  if [ "$PURGE" -eq 1 ]; then
    rm -rf "$CONF_DIR"
    say "Removed $CONF_DIR"
  else
    say "Kept $CONF_DIR (use --purge to delete it)"
  fi
  say "Kept /var/backups/nexacloud (your database backups)"
  say "NexaCloud removed. Your sites, nginx and PHP were not touched."
  exit 0
fi

# ---------------------------------------------------------------- checks
arch="$(uname -m)"
[ "$arch" = "x86_64" ] || die "only x86_64 is supported right now (found $arch)"
if [ -r /etc/os-release ]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  case "${ID:-}" in
    ubuntu|debian) ;;
    *) warn "tested on Ubuntu/Debian; found ${PRETTY_NAME:-unknown}. Continuing." ;;
  esac
fi
for bin in curl tar sha256sum; do
  command -v "$bin" >/dev/null || die "missing '$bin' (apt install $bin)"
done

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# ---------------------------------------------------------------- download
if [ -n "$TARBALL" ]; then
  say "Using local package $TARBALL"
  [ -f "$TARBALL" ] || die "file not found: $TARBALL"
  cp "$TARBALL" "$tmp/$ASSET"
  if [ -f "$TARBALL.sha256" ]; then
    cp "$TARBALL.sha256" "$tmp/$ASSET.sha256"
  else
    die "missing checksum file $TARBALL.sha256"
  fi
else
  if [ "$VERSION" = "latest" ]; then
    base="https://github.com/$REPO/releases/latest/download"
  else
    base="https://github.com/$REPO/releases/download/$VERSION"
  fi
  say "Downloading NexaCloud ($VERSION) from github.com/$REPO"
  curl -fsSL --retry 3 -o "$tmp/$ASSET" "$base/$ASSET" \
    || die "could not download $base/$ASSET — has a release been published yet?"
  curl -fsSL --retry 3 -o "$tmp/$ASSET.sha256" "$base/$ASSET.sha256" \
    || die "could not download the checksum file"
fi

say "Verifying checksum"
(cd "$tmp" && sha256sum -c "$ASSET.sha256" >/dev/null) || die "checksum mismatch — download is corrupt or tampered"

mkdir -p "$tmp/x"
tar -xzf "$tmp/$ASSET" -C "$tmp/x"
[ -x "$tmp/x/nexacloud/bin/nexacloud" ] || die "package is missing bin/nexacloud"
[ -f "$tmp/x/nexacloud/web/index.html" ] || die "package is missing web/index.html"

# ---------------------------------------------------------------- install
upgrading=0
if systemctl is-active --quiet nexacloud 2>/dev/null; then
  upgrading=1
  say "Existing install found — upgrading"
  systemctl stop nexacloud
fi

install -d -m 0755 "$PREFIX" "$PREFIX/bin"
install -m 0755 "$tmp/x/nexacloud/bin/nexacloud" "$PREFIX/bin/nexacloud"
rm -rf "$PREFIX/web.new"
cp -r "$tmp/x/nexacloud/web" "$PREFIX/web.new"
rm -rf "$PREFIX/web"
mv "$PREFIX/web.new" "$PREFIX/web"
ln -sf "$PREFIX/bin/nexacloud" /usr/local/bin/nexacloud
install -d -m 0700 "$CONF_DIR"

init_out="$("$PREFIX/bin/nexacloud" init --config "$CONF")"
password="$(printf '%s\n' "$init_out" | sed -n 's/^NEXACLOUD_ADMIN_PASSWORD=//p')"

cat > "$UNIT" <<EOF
[Unit]
Description=NexaCloud server panel agent
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=$PREFIX/bin/nexacloud serve --config $CONF
Restart=on-failure
RestartSec=3
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable nexacloud >/dev/null 2>&1
systemctl restart nexacloud

# ---------------------------------------------------------------- health
listen="$(sed -n 's/^listen *= *"\(.*\)"/\1/p' "$CONF" | head -1)"
port="${listen##*:}"; port="${port:-8443}"
scheme="https"
grep -Eq '^tls *= *false' "$CONF" && scheme="http"

say "Waiting for the agent to start"
ok=0
for _ in $(seq 1 30); do
  if curl -fsk "$scheme://127.0.0.1:$port/api/health" >/dev/null 2>&1; then ok=1; break; fi
  sleep 1
done
if [ "$ok" -ne 1 ]; then
  journalctl -u nexacloud -n 30 --no-pager >&2 || true
  die "the agent did not come up — see the log above"
fi

if command -v ufw >/dev/null && ufw status 2>/dev/null | grep -q '^Status: active'; then
  if ! ufw status | grep -Eq "^$port(/tcp)? +ALLOW"; then
    warn "UFW is active and port $port is not open. To reach the panel run:  ufw allow $port/tcp"
  fi
fi

ip="$(hostname -I 2>/dev/null | awk '{print $1}')"
ip="${ip:-YOUR_SERVER_IP}"

echo
printf '%sNexaCloud is running.%s\n\n' "$c_bold" "$c_off"
printf '  Open:      %s://%s:%s\n' "$scheme" "$ip" "$port"
if [ -n "$password" ]; then
  printf '  Password:  %s%s%s\n' "$c_bold" "$password" "$c_off"
  printf '             (shown once — save it now)\n'
elif [ "$upgrading" -eq 1 ]; then
  printf '  Password:  unchanged (upgrade)\n'
else
  printf '  Password:  unchanged (existing config kept)\n'
fi
echo
echo "  Your browser will warn about the certificate the first time: the panel uses"
echo "  a self-signed certificate until you give it a domain. Click 'Advanced → Proceed'."
echo
echo "  Forgot the password:  sudo nexacloud reset-password"
echo "  Upgrade:              re-run this installer"
echo "  Uninstall:            curl -fsSL https://raw.githubusercontent.com/$REPO/main/scripts/install.sh | sudo bash -s -- --uninstall"
echo
