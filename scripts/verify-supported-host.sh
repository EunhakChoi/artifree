#!/usr/bin/env bash
# The supported-host check of decision 0058 (R03fb): a clean install of the linux-x64 archive that
# follows docs/execution-host.md step by step and ends with a published react-v1 build served by
# the /b/ route.
#
# Usage: scripts/verify-supported-host.sh RELEASE_DIR TEMPLATE_DIR
#
# RELEASE_DIR holds the archive and its .sha256 file; TEMPLATE_DIR holds the bundler template
# (index.html, main.tsx, app.css). The script changes the host as an operator would: it routes
# core dumps to a plain file, tightens /opt, creates the account artifree, installs Bubblewrap and
# a tool root, loads an AppArmor profile and starts two systemd units. So it runs only on a
# disposable GitHub-hosted Ubuntu 24.04 x86_64 runner and refuses anywhere else; never run it on a
# contributor's machine (decision 0056 rule 12).
set -euo pipefail

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}
say() {
  printf '== %s\n' "$*"
}

(($# == 2)) || fail "usage: scripts/verify-supported-host.sh RELEASE_DIR TEMPLATE_DIR"
[[ "${GITHUB_ACTIONS:-}" == true && "${RUNNER_ENVIRONMENT:-}" == github-hosted ]] \
  || fail "this check changes the host; it runs only on a GitHub-hosted runner"
# shellcheck source=/dev/null # The host's own file.
. /etc/os-release
[[ "$ID" == ubuntu && "$VERSION_ID" == 24.04 && "$(uname -m)" == x86_64 ]] \
  || fail "the supported host is Ubuntu 24.04 on x86_64, not $ID $VERSION_ID on $(uname -m)"
release=$(cd -- "$1" && pwd)
template=$(cd -- "$2" && pwd)
checkout=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

shopt -s nullglob
archives=("$release"/artifree-*-linux-x64.tar.gz)
shopt -u nullglob
((${#archives[@]} == 1)) || fail "expected one linux-x64 archive in $release"
archive=${archives[0]}
name=$(basename -- "$archive" .tar.gz)

say "1. keep the parents of the tool root writable by root only, before anything is made under /opt"
# The hosted runner's /opt is mode 0777.
sudo chmod 0755 /opt

say "2. route core dumps to a plain file"
if systemctl cat apport.service > /dev/null 2>&1; then
  sudo systemctl disable --now apport.service
fi
printf 'kernel.core_pattern = core\n' | sudo tee /etc/sysctl.d/99-artifree-core.conf > /dev/null
sudo sysctl -p /etc/sysctl.d/99-artifree-core.conf > /dev/null
[[ "$(cat /proc/sys/kernel/core_pattern)" == core ]] || fail "kernel.core_pattern does not read core"

say "3. check and unpack the archive"
(cd -- "$release" && sha256sum -c -- "$name.tar.gz.sha256")
sudo install -d -o root -g root -m 0755 /opt/artifree /opt/artifree/releases
sudo tar -xzf "$archive" -C /opt/artifree/releases --no-same-owner
package=/opt/artifree/releases/$name
sudo ln -sfn -- "$package" /opt/artifree/current

say "4. prepare the tool root of this release"
root=$(sudo "$package/tools/package-root.sh" "$package/bin/artifree-server")
version=$("$package/bin/artifree-server" --version)
[[ "$root" == "/opt/artifree/tools/${version#artifree-server }" ]] \
  || fail "the script prepared $root for $version"

say "5. create the account and its directories"
sudo useradd --system --user-group --home-dir /var/lib/artifree --shell /usr/sbin/nologin artifree
sudo install -d -o artifree -g artifree -m 0700 /var/lib/artifree /var/lib/artifree/execution

say "6. install and start the two units"
sudo install -o root -g root -m 0644 -t /etc/systemd/system \
  "$package/tools/artifree.service" "$package/tools/artifree-execution.service"
sudo systemctl daemon-reload
sudo systemctl enable --now artifree.service
# The server answers /healthz once its database is open, so the worker then finds it.
for _ in $(seq 120); do
  curl --silent --fail --output /dev/null http://127.0.0.1:7410/healthz && break
  sleep 0.5
done
curl --silent --show-error --fail --output /dev/null http://127.0.0.1:7410/healthz \
  || fail "the server does not answer /healthz"
sudo test -f /var/lib/artifree/data/access-token || fail "the server made no access token"
sudo systemctl enable --now artifree-execution.service

say "7. readiness, then a react-v1 build of the template"
token_file=${RUNNER_TEMP:?}/artifree-access-token
sudo install -o "$(id -u)" -g "$(id -g)" -m 0600 /var/lib/artifree/data/access-token "$token_file"
node "$checkout/scripts/verify-supported-host.mjs" \
  --package "$package" --token-file "$token_file" --template "$template"
restarts=$(systemctl show --property=NRestarts --value artifree-execution.service)
[[ "$restarts" == 0 ]] || fail "the execution worker was restarted $restarts times"
say "the supported host published a build"
