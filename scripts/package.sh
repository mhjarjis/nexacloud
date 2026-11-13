#!/usr/bin/env bash
# Build the release tarball from an already-built agent and web UI.
#   agent/target/release/nexacloud   (cargo build --release)
#   web/.output/public               (npx nuxi generate)
# Output: dist/nexacloud-x86_64-linux.tar.gz (+ .sha256)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
bin="${NEXACLOUD_BIN:-$root/agent/target/release/nexacloud}"
web="$root/web/.output/public"
asset="nexacloud-x86_64-linux.tar.gz"

[ -x "$bin" ] || { echo "missing $bin — run: cargo build --release (in agent/)" >&2; exit 1; }
[ -f "$web/index.html" ] || { echo "missing $web/index.html — run: npx nuxi generate (in web/)" >&2; exit 1; }

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/nexacloud/bin"
cp "$bin" "$stage/nexacloud/bin/nexacloud"
cp -R "$web" "$stage/nexacloud/web"
cp "$root/scripts/install.sh" "$stage/nexacloud/install.sh"

mkdir -p "$root/dist"
tar_opts=()
if tar --version 2>/dev/null | grep -q bsdtar; then
  # macOS: don't embed Apple xattrs/resource forks (GNU tar warns about them)
  export COPYFILE_DISABLE=1
  tar_opts=(--no-xattrs --no-mac-metadata)
fi
tar ${tar_opts[@]+"${tar_opts[@]}"} -C "$stage" -czf "$root/dist/$asset" nexacloud
cd "$root/dist"
if command -v sha256sum >/dev/null; then
  sha256sum "$asset" > "$asset.sha256"
else
  shasum -a 256 "$asset" > "$asset.sha256"
fi
echo "built dist/$asset"
cat "$asset.sha256"
