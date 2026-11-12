#!/usr/bin/env bash
# Driver for the Linux enforcement job (decision 0056 rules 9, 10, 12, 13).
# Privileged steps go only through prepare.sh and systemd-run.
set -euo pipefail

# The tool root's fixed path. It must equal ROOT in prepare.sh and the provider's
# TOOL_ROOT (decision 0056 rule 6).
readonly ROOT=/opt/artifree/tools/v1
readonly OUT=target/enforcement
readonly FIXTURE=target/debug/examples/artifree-execution-fixture

SCRIPT_DIR="$(cd -- "$(dirname -- "$0")" && pwd)"
readonly SCRIPT_DIR
readonly PREPARE="$SCRIPT_DIR/prepare.sh"
cd -- "$SCRIPT_DIR/../.."

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

say() {
  printf '==> %s\n' "$*" >&2
}

# libtest prints a summary for a filtered run. A filter that matches nothing still exits
# 0, so the summary must show exactly one passed test.
require_one_passed() {
  grep -Fq 'test result: ok. 1 passed; 0 failed;' "$1" \
    || fail "$1 did not record exactly one passed test"
}

# run_direct LABEL TEST runs one ignored test as the runner, without privilege.
run_direct() {
  local label=$1 name=$2
  "$test_binary" --ignored --exact "$name" --test-threads=1 2>&1 | tee "$OUT/$label.log"
  require_one_passed "$OUT/$label.log"
}

# run_unit LABEL TEST [systemd-run option...] [-- wrapper...] runs one ignored test in a
# transient unit as the runner's account. The wrapper, when given, is the unit's command
# before the test binary.
run_unit() {
  local label=$1 name=$2
  shift 2
  local -a unit_options=() wrapper=()
  while (($# > 0)) && [[ $1 != -- ]]; do
    unit_options+=("$1")
    shift
  done
  if (($# > 0)); then
    shift
    wrapper=("$@")
  fi
  sudo systemd-run --quiet --wait --pipe --collect --service-type=exec \
    --uid="$(id -u)" --gid="$(id -g)" --working-directory="$PWD" \
    --setenv="ARTIFREE_ENFORCEMENT_REPORT=$PWD/$OUT/host-report.txt" \
    "${unit_options[@]}" -- "${wrapper[@]}" \
    "$test_binary" --ignored --exact "$name" --test-threads=1 2>&1 | tee "$OUT/$label.log"
  require_one_passed "$OUT/$label.log"
}

mkdir -p -- "$OUT"

# The environment is written before the kernel check, so a failed run still records the
# kernel it ran on.
release="$(uname -r)"
{
  printf 'kernel release: %s\n' "$release"
  printf 'machine: %s\n' "$(uname -m)"
} > "$OUT/environment.txt"
case "$release" in
  7.0.*) ;;
  *) fail "kernel $release is not the reviewed 7.0 release" ;;
esac

say "build the fixture and find the enforcement test binary"
cargo build -p artifree-providers --example artifree-execution-fixture --locked
[[ -f "$FIXTURE" && -x "$FIXTURE" ]] || fail "the fixture was not built: $FIXTURE"
metadata="$(cargo test -p artifree-providers --test enforcement --no-run --locked --message-format=json)"
test_binaries="$(printf '%s\n' "$metadata" \
  | jq -r 'select(.reason == "compiler-artifact" and .target.name == "enforcement" and .executable != null) | .executable')"
case "$test_binaries" in
  "") fail "cargo reported no enforcement test binary" ;;
  *$'\n'*) fail "cargo reported more than one enforcement test binary" ;;
esac
test_binary=$test_binaries
[[ -x "$test_binary" ]] || fail "the enforcement test binary is not executable: $test_binary"

say "part 1: install the pinned launcher and assemble the tool root"
sudo "$PREPARE" install "$FIXTURE"
{
  printf 'bubblewrap package: %s\n' "$(dpkg-query -W -f "\${Version}" bubblewrap)"
  printf 'launcher version: %s\n' "$("$ROOT/bin/bwrap" --version)"
  printf 'launcher sha256: %s\n' "$(sha256sum -- "$ROOT/bin/bwrap")"
} >> "$OUT/environment.txt"

say "part 2: the host as found, before any preparation"
sudo "$PREPARE" save-routing
run_direct as-found job_unprepared_host_is_refused

say "part 3: prepare the host, then run the matrix in a delegated unit"
sudo "$PREPARE" profile-on
sudo "$PREPARE" routing-file
run_unit prepared job_prepared_host_passes_every_facility -p Delegate=yes

say "rule 10: remove each condition in turn and require the refusal that names it"
sudo "$PREPARE" routing-pipe
run_unit removed-routing job_pipe_routing_is_refused -p Delegate=yes
sudo "$PREPARE" routing-file

sudo "$PREPARE" profile-off
run_unit removed-profile job_missing_profile_is_refused -p Delegate=yes
sudo "$PREPARE" profile-on

# The memory controller is removed through a runtime slice, so that the unit's own cgroup
# lacks it.
sudo "$PREPARE" nomem-slice-on
run_unit removed-controller job_missing_controller_is_refused -p Delegate=yes \
  -p Slice=artifree-enforcement-nomem.slice
sudo "$PREPARE" nomem-slice-off

run_unit removed-delegation job_undelegated_group_is_refused

# The wrapper shell occupies the delegated group and then execs the test binary, so the
# provider's own launch path is not involved.
run_unit occupied-group job_occupied_group_is_refused -p Delegate=yes \
  -- /bin/sh -c '/bin/sleep 600 & exec "$0" "$@"'

sudo "$PREPARE" launcher-away
run_unit removed-launcher job_missing_launcher_is_refused -p Delegate=yes
sudo "$PREPARE" launcher-back

sudo "$PREPARE" fixture-away
run_unit removed-fixture job_missing_fixture_is_refused -p Delegate=yes
sudo "$PREPARE" fixture-back

say "restore the routing and the profile"
sudo "$PREPARE" routing-pipe
sudo "$PREPARE" profile-off
say "enforcement checks completed"
