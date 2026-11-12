#!/usr/bin/env bash
# Driver for the Linux enforcement job (decision 0056 rules 9, 10, 12, 13).
# Privileged steps go only through prepare.sh and systemd-run.
set -euo pipefail

# The tool root's fixed path. It must equal ROOT in prepare.sh and the provider's
# TOOL_ROOT (decision 0056 rule 6).
readonly ROOT=/opt/artifree/tools/v1
readonly OUT=target/enforcement
readonly FIXTURE=target/debug/examples/artifree-execution-fixture
readonly PAYLOAD=target/debug/examples/artifree-execution-payload

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

# run_direct BINARY LABEL TEST runs one ignored test as the runner, without privilege.
run_direct() {
  local binary=$1 label=$2 name=$3
  "$binary" --ignored --exact "$name" --test-threads=1 2>&1 | tee "$OUT/$label.log"
  require_one_passed "$OUT/$label.log"
}

# run_unit BINARY LABEL TEST [systemd-run option...] [-- wrapper...] runs one ignored test in
# a transient unit as the runner's account. The wrapper, when given, is the unit's command
# before the test binary.
run_unit() {
  local binary=$1 label=$2 name=$3
  shift 3
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
    --setenv="ARTIFREE_ATTEMPT_REPORT=$PWD/$OUT/attempt-report.txt" \
    "${unit_options[@]}" -- "${wrapper[@]}" \
    "$binary" --ignored --exact "$name" --test-threads=1 2>&1 | tee "$OUT/$label.log"
  require_one_passed "$OUT/$label.log"
}

# run_scenario BINARY SCENARIO LABEL TEST sets the payload scenario, runs one job test in a
# delegated unit, and then requires that no payload process is left on the host. systemd kills
# what is left of a unit when the unit stops, so this check shows only that nothing escaped the
# unit. The evidence that the launch itself left nothing is the job test's own: it lists the
# attempt groups and scans /proc after the launch returns, while the unit still runs.
run_scenario() {
  local binary=$1 scenario=$2 label=$3 name=$4 left status=0
  sudo "$PREPARE" scenario "$scenario"
  run_unit "$binary" "$label" "execution::attempt::resource_tests::$name" -p Delegate=yes
  # pgrep exits 1 when nothing matches; any other status is a failure of the check itself.
  left="$(pgrep -a -f artifree-execution-payload)" || status=$?
  case "$status" in
    0)
      printf '%s\n' "$left" >&2
      fail "a payload process outlived $name"
      ;;
    1) ;;
    *) fail "the payload process check failed with status $status" ;;
  esac
}

# This script changes the host's crash routing, its AppArmor profiles and /opt. It is for
# the job's disposable runner only (decision 0056 rule 12).
[[ ${GITHUB_ACTIONS:-} == true ]] \
  || fail "this script prepares a disposable CI runner; it does not run on any other host"

# After a failed step the conditions it removed are put back, so the host is not left with
# the job's routing, its profile, a changed, extra, missing or moved tool file, or a payload
# scenario other than exit. Each restore may fail on its own. The kernel log lines about the
# failure are printed first, for diagnosis.
restore_host() {
  local status=$?
  if ((status != 0)); then
    sudo "$PREPARE" kernel-log || true
    sudo "$PREPARE" launcher-back 2> /dev/null || true
    sudo "$PREPARE" fixture-back 2> /dev/null || true
    sudo "$PREPARE" nomem-slice-off 2> /dev/null || true
    sudo "$PREPARE" tool-restore 2> /dev/null || true
    sudo "$PREPARE" tool-extra-off 2> /dev/null || true
    sudo "$PREPARE" tool-missing-off 2> /dev/null || true
    sudo "$PREPARE" manifest-back 2> /dev/null || true
    sudo "$PREPARE" payload-restore 2> /dev/null || true
    sudo "$PREPARE" scenario exit 2> /dev/null || true
    sudo "$PREPARE" profile-off 2> /dev/null || true
    sudo "$PREPARE" routing-pipe 2> /dev/null || true
  fi
  exit "$status"
}
trap restore_host EXIT

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

say "build the test-support programs and find the test binaries"
cargo build -p artifree-providers --example artifree-execution-fixture --locked
[[ -f "$FIXTURE" && -x "$FIXTURE" ]] || fail "the fixture was not built: $FIXTURE"
cargo build -p artifree-providers --example artifree-execution-payload --locked
[[ -f "$PAYLOAD" && -x "$PAYLOAD" ]] || fail "the payload was not built: $PAYLOAD"
metadata="$(cargo test -p artifree-providers --test enforcement --no-run --locked --message-format=json)"
test_binaries="$(printf '%s\n' "$metadata" \
  | jq -r 'select(.reason == "compiler-artifact" and .target.name == "enforcement" and .executable != null) | .executable')"
case "$test_binaries" in
  "") fail "cargo reported no enforcement test binary" ;;
  *$'\n'*) fail "cargo reported more than one enforcement test binary" ;;
esac
test_binary=$test_binaries
[[ -x "$test_binary" ]] || fail "the enforcement test binary is not executable: $test_binary"
lib_metadata="$(cargo test -p artifree-providers --lib --no-run --locked --message-format=json)"
lib_binaries="$(printf '%s\n' "$lib_metadata" \
  | jq -r 'select(.reason == "compiler-artifact" and (.target.kind | index("lib")) != null and .executable != null) | .executable')"
case "$lib_binaries" in
  "") fail "cargo reported no library test binary" ;;
  *$'\n'*) fail "cargo reported more than one library test binary" ;;
esac
lib_binary=$lib_binaries
[[ -x "$lib_binary" ]] || fail "the library test binary is not executable: $lib_binary"

say "part 1: install the pinned launcher and assemble the tool root"
sudo "$PREPARE" install "$FIXTURE" "$PAYLOAD"
{
  printf 'bubblewrap package: %s\n' "$(dpkg-query -W -f "\${Version}" bubblewrap)"
  printf 'launcher version: %s\n' "$("$ROOT/bin/bwrap" --version)"
  printf 'launcher sha256: %s\n' "$(sha256sum -- "$ROOT/bin/bwrap")"
  printf 'tool manifest sha256: %s\n' "$(sha256sum -- "$ROOT/MANIFEST")"
} >> "$OUT/environment.txt"

say "part 2: the host as found, before any preparation"
sudo "$PREPARE" save-routing
run_direct "$test_binary" as-found job_unprepared_host_is_refused

say "part 3: prepare the host, then run the matrix in a delegated unit"
sudo "$PREPARE" profile-on
sudo "$PREPARE" routing-file
run_unit "$test_binary" prepared job_prepared_host_passes_every_facility -p Delegate=yes

say "rule 10: remove each condition in turn and require the refusal that names it"
sudo "$PREPARE" routing-pipe
run_unit "$test_binary" removed-routing job_pipe_routing_is_refused -p Delegate=yes
sudo "$PREPARE" routing-file

sudo "$PREPARE" profile-off
run_unit "$test_binary" removed-profile job_missing_profile_is_refused -p Delegate=yes
sudo "$PREPARE" profile-on

# The memory controller is removed through a runtime slice, so that the unit's own cgroup
# lacks it.
sudo "$PREPARE" nomem-slice-on
run_unit "$test_binary" removed-controller job_missing_controller_is_refused -p Delegate=yes \
  -p Slice=artifree-enforcement-nomem.slice
sudo "$PREPARE" nomem-slice-off

run_unit "$test_binary" removed-delegation job_undelegated_group_is_refused

# The wrapper shell occupies the delegated group and then execs the test binary, so the
# provider's own launch path is not involved.
run_unit "$test_binary" occupied-group job_occupied_group_is_refused -p Delegate=yes \
  -- /bin/sh -c '/bin/sleep 600 & exec "$0" "$@"'

sudo "$PREPARE" launcher-away
run_unit "$test_binary" removed-launcher job_missing_launcher_is_refused -p Delegate=yes
sudo "$PREPARE" launcher-back

sudo "$PREPARE" fixture-away
run_unit "$test_binary" removed-fixture job_missing_fixture_is_refused -p Delegate=yes
sudo "$PREPARE" fixture-back

say "part 4: the attempt, its admission lock and the syscall table"
run_direct "$lib_binary" syscall-table execution::policy::tests::job_syscall_table_matches_the_kernel_headers
run_unit "$lib_binary" attempt execution::attempt::tests::job_fixture_attempt_runs_in_its_group_and_exits -p Delegate=yes
run_unit "$lib_binary" false-ready execution::attempt::tests::job_false_ready_is_never_released -p Delegate=yes
run_unit "$lib_binary" stalled-start execution::attempt::tests::job_stalled_start_is_killed_at_the_readiness_deadline -p Delegate=yes
run_unit "$lib_binary" wrong-release execution::attempt::tests::job_wrong_release_byte_starts_no_payload -p Delegate=yes
run_unit "$lib_binary" unknown-identity execution::attempt::tests::job_unknown_identity_starts_no_launcher -p Delegate=yes

# The payload is replaced by a text file, so only the exec of the payload can fail. The
# manifest is rewritten for the replacement, so admission still passes.
sudo "$PREPARE" payload-break
run_unit "$lib_binary" exec-failure execution::attempt::tests::job_payload_exec_failure_counts_as_not_started -p Delegate=yes
sudo "$PREPARE" payload-restore

run_unit "$lib_binary" lock-busy execution::attempt::tests::job_second_attempt_is_refused_while_the_lock_is_held -p Delegate=yes

say "part 5: each change to the tool root is refused by name, and is put back afterwards"
sudo "$PREPARE" tool-modify
run_unit "$test_binary" modified-tool job_modified_tool_is_refused -p Delegate=yes
sudo "$PREPARE" tool-restore

sudo "$PREPARE" tool-extra-on
run_unit "$test_binary" extra-tool job_extra_tool_file_is_refused -p Delegate=yes
sudo "$PREPARE" tool-extra-off

sudo "$PREPARE" tool-missing-on
run_unit "$test_binary" missing-tool job_missing_tool_file_is_refused -p Delegate=yes
sudo "$PREPARE" tool-missing-off

sudo "$PREPARE" manifest-away
run_unit "$test_binary" missing-manifest job_missing_manifest_is_refused -p Delegate=yes
sudo "$PREPARE" manifest-back

say "part 6: resources and cleanup under the launch"
run_scenario "$lib_binary" threads resource-threads job_thread_bomb_stops_at_the_task_limit
run_scenario "$lib_binary" processes resource-processes job_process_bomb_stops_at_the_task_limit
run_scenario "$lib_binary" memory resource-memory job_memory_hog_is_killed_with_its_group
run_scenario "$lib_binary" scratch-bytes resource-scratch-bytes job_scratch_bytes_are_bounded_and_nothing_else_is_writable
run_scenario "$lib_binary" scratch-inodes resource-scratch-inodes job_scratch_inodes_are_bounded
run_scenario "$lib_binary" flood resource-flood job_log_flood_stops_at_the_output_budget
run_scenario "$lib_binary" hang resource-hang job_hang_is_killed_at_the_wall_deadline
run_scenario "$lib_binary" cpu resource-cpu job_cpu_spinners_share_one_cpu
run_scenario "$lib_binary" survivors resource-survivors job_background_processes_do_not_outlive_a_normal_exit
run_scenario "$lib_binary" survivors-hang resource-survivors-hang job_background_processes_do_not_outlive_the_deadline
sudo "$PREPARE" scenario exit

say "restore the routing and the profile"
sudo "$PREPARE" routing-pipe
sudo "$PREPARE" profile-off
say "enforcement checks completed"
