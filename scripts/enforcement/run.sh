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

# counter NAME prints the line count that counters-show reports for core.NAME.
counter() {
  sudo "$PREPARE" counters-show | awk -F= -v key="core.$1" '$1 == key { print $2 }'
}

# trusted_crash NAME makes one crash with a core limit of 0 as the runner, outside any attempt,
# and requires the counter core.NAME to go from 0 to 1. The collector counts asynchronously, so the
# counter is polled for up to 5 s (50 tries of 0.1 s) instead of being read after a fixed wait.
# It records both values (rule 13).
trusted_crash() {
  local name=$1 before after tries
  before="$(counter "$name")"
  [[ $before == 0 ]] || fail "the $name counter is $before before the trusted crash, not 0"
  (cd "$(mktemp -d)" && sh -c 'ulimit -c 0; kill -SEGV $$') || true
  after="$(counter "$name")"
  for ((tries = 0; tries < 50; tries++)); do
    [[ $after == 1 ]] && break
    sleep 0.1
    after="$(counter "$name")"
  done
  [[ $after == 1 ]] || fail "the $name counter is $after after 5 s of polling following the trusted crash, not 1"
  printf 'core.%s.before=%s\ncore.%s.after=%s\n' "$name" "$before" "$name" "$after" >> "$OUT/environment.txt"
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

# run_in_scenario BINARY MODULE SCENARIO LABEL TEST [systemd-run option...] sets the payload
# scenario, runs one job test of execution::attempt::MODULE in a delegated unit, and then requires
# that no payload or probe process is left on the host. The extra systemd-run options are passed
# to the unit after `-p Delegate=yes`. systemd kills what is left of a unit when the unit stops, so
# this check shows only that nothing escaped the unit. The evidence that the launch itself left
# nothing is the job test's own: it lists the attempt groups and scans /proc after the launch
# returns, while the unit still runs.
run_in_scenario() {
  local binary=$1 module=$2 scenario=$3 label=$4 name=$5 left status=0
  shift 5
  sudo "$PREPARE" scenario "$scenario"
  run_unit "$binary" "$label" "execution::attempt::$module::$name" -p Delegate=yes "$@"
  # pgrep exits 1 when nothing matches; any other status is a failure of the check itself.
  left="$(pgrep -a -f 'artifree-execution-payload|/tools/probes/|/tools/v1/probes/')" || status=$?
  case "$status" in
    0)
      printf '%s\n' "$left" >&2
      fail "a payload or probe process outlived $name"
      ;;
    1) ;;
    *) fail "the payload or probe process check failed with status $status" ;;
  esac
}

# run_scenario BINARY SCENARIO LABEL TEST runs one resource test (execution::attempt::
# resource_tests) through run_in_scenario.
run_scenario() {
  run_in_scenario "$1" resource_tests "$2" "$3" "$4"
}

# run_server_job SCENARIO LABEL TEST [systemd-run option...] sets the payload scenario, runs one job
# test of the server's execution_jobs in a delegated unit and requires that no payload, probe or
# entry point process outlived it (R02bb, decision 0057).
run_server_job() {
  local scenario=$1 label=$2 name=$3 left status=0
  shift 3
  sudo "$PREPARE" scenario "$scenario"
  run_unit "$server_test" "$label" "$name" -p Delegate=yes "$@"
  # pgrep exits 1 when nothing matches; any other status is a failure of the check itself.
  left="$(pgrep -a -f 'artifree-execution-payload|/tools/probes/|/tools/v1/probes/|artifree-server execution')" || status=$?
  case "$status" in
    0)
      printf '%s\n' "$left" >&2
      fail "a payload, probe or entry point process outlived $name"
      ;;
    1) ;;
    *) fail "the payload or probe process check failed with status $status" ;;
  esac
}

# death_at PHASE GUARDIAN SCENARIO [HOLD_DIR] kills the supervisor of a guardian test at PHASE and checks what it
# leaves (R02ah, decision 0056, amendment of 2026-11-13, the guardian). A test must run the supervisor
# in a unit with `KillMode=process`, or the service manager does the guardian's work and the test
# proves nothing. Within 10 s no payload, probe or guardian process may be left. Each attempt group
# beneath the unit must be empty, and there may be at most one, or none at collecting. systemd trims
# the empty groups of a delegated subtree when the unit stops (measured in the job of #205: at guarded
# none was left), and the kernel removes only an empty group, so a removed group was empty too. With
# GUARDIAN=1 and any phase but collecting, the guardian's line must name supervisor-gone with emptied=1. Otherwise no guardian line may appear. The unit is then removed.
# With HOLD_DIR the supervisor uses that execution directory, which run.sh made and removes, so that a second
# unit can reconcile it (R02ai). At registered and created no guardian exists.
death_at() {
  local phase=$1 guardian=$2 scenario=$3
  local hold_dir=${4:-}
  local unit="artifree-supervisor-$phase-$guardian"
  local markers="$OUT/death-$phase-$guardian"
  local log="$OUT/death-$phase-$guardian.log"
  local group="/sys/fs/cgroup/system.slice/$unit.service"
  local pattern='artifree-execution-payload|/tools/probes/|/tools/v1/probes/|artifree-execution-fixture guardian'
  local unit_pid main tries status=0 populated left
  local -a groups=()
  local -a hold_option=()
  if [[ -n $hold_dir ]]; then hold_option=(--setenv="ARTIFREE_HOLD_DIR=$PWD/$hold_dir"); fi
  sudo "$PREPARE" scenario "$scenario"
  rm -rf -- "$markers"
  mkdir -p -- "$markers"
  # The subshell, which waits for sudo without exec, is the runner's own process, so `kill -0` can
  # probe it; sudo's process is root's, where `kill -0` fails with EPERM whether it runs or not.
  (
    sudo systemd-run --quiet --wait --pipe --collect --service-type=exec --unit="$unit" \
      -p Delegate=yes -p KillMode=process \
      --uid="$(id -u)" --gid="$(id -g)" --working-directory="$PWD" \
      --setenv="ARTIFREE_HOLD_PHASE=$phase" --setenv="ARTIFREE_HOLD_GUARDIAN=$guardian" \
      --setenv="ARTIFREE_HOLD_MARKERS=$PWD/$markers" \
      "${hold_option[@]}" \
      "$lib_binary" --ignored --exact execution::attempt::guardian_tests::job_supervisor_holds_at_its_phase \
      --test-threads=1
  ) > "$log" 2>&1 &
  unit_pid=$!
  for ((tries = 0; tries < 600; tries++)); do
    [[ -e "$markers/$phase" ]] && break
    kill -0 "$unit_pid" 2> /dev/null || break
    sleep 0.1
  done
  [[ -e "$markers/$phase" ]] || fail "the supervisor wrote no $phase marker within 60 s (guardian=$guardian)"
  main="$(systemctl show -p MainPID --value "$unit.service")"
  [[ $main =~ ^[1-9][0-9]*$ ]] || fail "the unit $unit has no main process: $main"
  sudo kill -KILL "$main"
  for ((tries = 0; tries < 100; tries++)); do
    status=0
    pgrep -f "$pattern" > /dev/null || status=$?
    ((status == 1)) && break
    ((status == 0)) || fail "the process check after the death at $phase failed with status $status"
    sleep 0.1
  done
  if ((status != 1)); then
    pgrep -a -f "$pattern" >&2 || true
    fail "a payload, probe or guardian process outlived the supervisor's death at $phase (guardian=$guardian)"
  fi
  # The guardian's line is matched without an anchor, because libtest's `test ... ` line may precede it
  # on the same line.
  # The provider places the attempt group beside `supervisor`, directly beneath the unit's group, and
  # names it attempt-<digits>-<digits>. The cgroup file system is readable without privilege.
  for left in "$group"/attempt-*; do
    [[ -d "$left" && ${left##*/} =~ ^attempt-[0-9]+-[0-9]+$ ]] && groups+=("$left")
  done
  printf 'death-at phase=%s guardian=%s groups-left=%d\n' "$phase" "$guardian" "${#groups[@]}"
  if [[ $phase == collecting ]]; then
    ((${#groups[@]} == 0)) || fail "an attempt group is left after the death at collecting: ${groups[*]}"
  else
    ((${#groups[@]} <= 1)) || fail "the death at $phase (guardian=$guardian) left ${#groups[@]} attempt groups, not at most 1"
    for left in "${groups[@]}"; do
      # The kernel may report the last exit a moment after the processes are gone, so the check is
      # retried for up to 2 s. A group that systemd removed meanwhile was empty.
      for ((tries = 0; tries < 20; tries++)); do
        [[ -e "$left/cgroup.events" ]] || break
        populated="$(grep '^populated ' "$left/cgroup.events" || true)"
        [[ $populated == 'populated 0' ]] && break
        sleep 0.1
      done
      [[ ! -e "$left/cgroup.events" || $populated == 'populated 0' ]] || fail "the attempt group after the death at $phase is not empty: $populated"
    done
  fi
  if [[ $guardian == 1 && $phase != collecting ]]; then
    grep -q 'guardian supervisor-gone emptied=1' "$log" || fail "the guardian reported no supervisor-gone emptied=1 line after the death at $phase"
  elif grep -Eq 'guardian (supervisor-gone|renewal-expired|deadline) emptied=' "$log"; then
    fail "a guardian line appeared after the death at $phase (guardian=$guardian)"
  fi
  sudo "$PREPARE" supervisor-clear "$unit"
  sudo systemctl stop "$unit.service" 2> /dev/null || true
  sudo systemctl reset-failed "$unit.service" 2> /dev/null || true
  wait "$unit_pid" || true
}

# routing_change LABEL MARKERS [systemd-run option...] runs the routing-change test of R02ag (part 10,
# decision 0056, amendment of 2026-11-13). The test's linger attempt runs in a unit while this script waits
# for its attempt group, sets the pipe routing, waits for the test's first marker, restores the file routing
# and then requires the test to pass. The extra systemd-run options are passed to the unit after the test's own.
routing_change() {
  local label=$1 markers=$2 change_pid seen leftover tries
  shift 2
  mkdir -p -- "$markers"
  # The wait below takes the first attempt group it finds, so none may be left from an earlier test.
  leftover="$(find /sys/fs/cgroup -maxdepth 8 -type d -name 'attempt-*' -print -quit 2> /dev/null || true)"
  [[ -z $leftover ]] || fail "an attempt group is left before the routing change: $leftover"
  run_in_scenario "$lib_binary" crash_tests linger "$label" job_a_routing_change_stops_the_attempt -p LimitCORE=1M \
    --setenv="ARTIFREE_ROUTING_MARKERS=$PWD/$markers" "${record_option[@]}" "$@" &
  change_pid=$!
  seen=""
  for ((tries = 0; tries < 300; tries++)); do
    seen="$(find /sys/fs/cgroup -maxdepth 8 -type d -name 'attempt-*' -print -quit 2> /dev/null || true)"
    [[ -n $seen ]] && break
    kill -0 "$change_pid" 2> /dev/null || break
    sleep 0.1
  done
  [[ -n $seen ]] || fail "no attempt group appeared within 30 s for the routing change"
  sudo "$PREPARE" routing-counter-pipe
  for ((tries = 0; tries < 600; tries++)); do
    [[ -e "$markers/first" ]] && break
    kill -0 "$change_pid" 2> /dev/null || break
    sleep 0.1
  done
  [[ -e "$markers/first" ]] || fail "the routing change test wrote no first marker within 60 s"
  sudo "$PREPARE" routing-file
  touch -- "$markers/restored"
  wait "$change_pid" || fail "the routing change test failed"
}

# This script changes the host's crash routing, its AppArmor profiles, /opt, and the kernel
# log's access and rate limit (`kernel.dmesg_restrict`, `kernel.printk_ratelimit`). It is for
# the job's disposable runner only (decision 0056 rule 12).
[[ ${GITHUB_ACTIONS:-} == true ]] \
  || fail "this script prepares a disposable CI runner; it does not run on any other host"

# After a failed step the conditions it removed are put back, so the host is not left with
# the job's routing, its profile, a changed, extra, missing or moved tool file, a payload
# scenario other than exit, or the core listener. Each restore may fail on its own. The kernel
# log lines about the failure are printed first, for diagnosis.
restore_host() {
  local status=$?
  if ((status != 0)); then
    sudo "$PREPARE" kernel-log || true
    sudo systemctl stop 'artifree-supervisor-*' 2> /dev/null || true
    sudo "$PREPARE" launcher-back 2> /dev/null || true
    sudo "$PREPARE" fixture-back 2> /dev/null || true
    sudo "$PREPARE" nomem-slice-off 2> /dev/null || true
    sudo "$PREPARE" tool-restore 2> /dev/null || true
    sudo "$PREPARE" tool-extra-off 2> /dev/null || true
    sudo "$PREPARE" tool-missing-off 2> /dev/null || true
    sudo "$PREPARE" manifest-back 2> /dev/null || true
    sudo "$PREPARE" payload-restore 2> /dev/null || true
    sudo "$PREPARE" decoy-off 2> /dev/null || true
    sudo "$PREPARE" scenario exit 2> /dev/null || true
    sudo "$PREPARE" listener-stop 2> /dev/null || true
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
# The server and its job tests are built with the enforcement-fixture feature, which alone makes
# the server serve the fixture profile (decision 0057 rule 2).
server_metadata="$(cargo test -p artifree-server --features enforcement-fixture --test execution_jobs --no-run --locked --message-format=json)"
server_test_binaries="$(printf '%s\n' "$server_metadata" \
  | jq -r 'select(.reason == "compiler-artifact" and .target.name == "execution_jobs" and .executable != null) | .executable')"
case "$server_test_binaries" in
  "") fail "cargo reported no execution_jobs test binary" ;;
  *$'\n'*) fail "cargo reported more than one execution_jobs test binary" ;;
esac
server_test=$server_test_binaries
[[ -x "$server_test" ]] || fail "the execution_jobs test binary is not executable: $server_test"
server_bin_binaries="$(printf '%s\n' "$server_metadata" \
  | jq -r 'select(.reason == "compiler-artifact" and .target.name == "artifree-server" and (.target.kind | index("bin")) != null and .executable != null) | .executable')"
case "$server_bin_binaries" in
  "") fail "cargo reported no artifree-server binary" ;;
  *$'\n'*) fail "cargo reported more than one artifree-server binary" ;;
esac
server_bin=$server_bin_binaries
[[ -x "$server_bin" ]] || fail "the artifree-server binary is not executable: $server_bin"

say "part 1: install the pinned launcher and assemble the tool root"
sudo "$PREPARE" install "$FIXTURE" "$PAYLOAD" "$server_bin"
{
  printf 'bubblewrap package: %s\n' "$(dpkg-query -W -f "\${Version}" bubblewrap)"
  printf 'launcher version: %s\n' "$("$ROOT/bin/bwrap" --version)"
  printf 'launcher sha256: %s\n' "$(sha256sum -- "$ROOT/bin/bwrap")"
  printf 'server sha256: %s\n' "$(sha256sum -- "$ROOT/bin/artifree-server")"
  printf 'tool manifest sha256: %s\n' "$(sha256sum -- "$ROOT/MANIFEST")"
} >> "$OUT/environment.txt"

say "part 2: the host as found, before any preparation"
sudo "$PREPARE" save-routing
run_direct "$test_binary" as-found job_unprepared_host_is_refused

say "part 3: prepare the host, then run the matrix in a delegated unit"
sudo "$PREPARE" profile-on
sudo "$PREPARE" routing-file
# The two kernel-log settings are recorded among the run's environment (rule 13). pipefail makes
# a refusal by the preparation fail the run.
sudo "$PREPARE" kernel-log-access | tee -a "$OUT/environment.txt"
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

say "part 7: the syscall mapping and the descriptor ceiling"
run_in_scenario "$lib_binary" mapping_tests probes mapping-probes job_probes_are_refused_with_their_records
run_in_scenario "$lib_binary" mapping_tests clone3 mapping-clone3 job_clone3_is_refused_with_enosys
# The harness also runs the clone3 control probe on the runner outside the sandbox; it is the
# only probe that runs there (decision 0056 rule 9, as amended).
run_in_scenario "$lib_binary" mapping_tests exit mapping-clone3-control job_clone3_control_outside_the_sandbox
run_in_scenario "$lib_binary" mapping_tests thread-refusal mapping-thread-refusal job_a_thread_is_refused_with_its_record
run_in_scenario "$lib_binary" mapping_tests descendant-refusal mapping-descendant-refusal job_a_descendants_limit_cannot_be_set
run_in_scenario "$lib_binary" mapping_tests lower mapping-lower job_lowering_the_soft_limit_is_refused
run_in_scenario "$lib_binary" mapping_tests descriptors mapping-descriptors job_descriptors_stop_at_the_ceiling
run_in_scenario "$lib_binary" mapping_tests watch mapping-watch job_the_boundary_holds_in_every_task
sudo "$PREPARE" scenario exit

say "part 8: files, descriptors and network"
# The job tests append their observations to the environment record, which is an artifact of the
# job (decision 0056 rule 13). The record is the file that the start of this script writes.
record_option=(--setenv="ARTIFREE_ENFORCEMENT_RECORD=$PWD/$OUT/environment.txt")
run_in_scenario "$lib_binary" isolation_tests network isolation-network job_the_network_reaches_no_host_listener "${record_option[@]}"
# The host test's canary process runs on the host outside the test's unit, because the provider
# refuses a group that holds another process (OccupiedCgroup). The test checks it through /proc.
sleep 300 &
canary_pid=$!
run_in_scenario "$lib_binary" isolation_tests host isolation-host job_host_files_and_processes_stay_intact \
  "${record_option[@]}" --setenv="ARTIFREE_CANARY_PID=$canary_pid"
kill -0 "$canary_pid" || fail "the canary process ended during the host test"
kill "$canary_pid"
wait "$canary_pid" || true
run_in_scenario "$lib_binary" isolation_tests proc-dev isolation-proc-dev job_proc_and_dev_pts_stay_inside "${record_option[@]}"
run_in_scenario "$lib_binary" isolation_tests environment isolation-leaked-descriptor job_a_leaked_descriptor_is_refused "${record_option[@]}"
# The injection check (decision 0056, amendment of 2026-11-13): the repository's own preload
# canary, named in LD_PRELOAD for this unit only, with LD_LIBRARY_PATH and BASH_ENV set too.
command -v gcc > /dev/null || fail "gcc is missing, so the preload canary of the injection check cannot be built"
gcc -shared -fPIC -O1 -o "$OUT/preload-canary.so" scripts/enforcement/preload-canary.c
rm -f -- /tmp/artifree-enforcement-preload.log
run_in_scenario "$lib_binary" isolation_tests environment isolation-environment job_the_environment_reaches_the_payload_empty \
  --setenv="LD_PRELOAD=$PWD/$OUT/preload-canary.so" \
  --setenv=LD_LIBRARY_PATH=/tmp/artifree-injected \
  --setenv=BASH_ENV=/tmp/artifree-injected.sh \
  "${record_option[@]}"
run_in_scenario "$lib_binary" isolation_tests marker isolation-marker job_no_file_outlives_an_attempt
# Rule 13: the environment record must hold the listener counters and the canaries' observations.
# A record without them fails the run, so the artifact of the job is never missing them.
grep -q '^listener\.' "$OUT/environment.txt" || fail "the environment record holds no listener counter"
grep -q '^canary\.' "$OUT/environment.txt" || fail "the environment record holds no canary observation"
sudo "$PREPARE" scenario exit

say "part 9: input and output"
# R02af (decision 0056, amendment of 2026-11-13): the staged input, the keeper and collection.
run_in_scenario "$lib_binary" io_tests copy io-copy job_input_becomes_the_expected_output "${record_option[@]}"
run_in_scenario "$lib_binary" io_tests input-write io-input-write job_the_input_cannot_be_changed "${record_option[@]}"
# One test per hostile output: each names its refusal reason, and its scenario holds one hostile feature.
for scenario in out-socket out-many out-big-file out-big-total out-bomb out-deep out-invalid-name \
  out-collision out-extension out-no-entry; do
  run_in_scenario "$lib_binary" io_tests "$scenario" "io-$scenario" "job_output_${scenario//-/_}_is_refused" "${record_option[@]}"
done
run_in_scenario "$lib_binary" io_tests out-plain io-planted job_planted_entries_are_refused "${record_option[@]}"
run_in_scenario "$lib_binary" io_tests out-fail io-failed job_a_failed_attempt_is_not_collected "${record_option[@]}"
run_in_scenario "$lib_binary" io_tests copy io-fresh job_a_fresh_attempt_succeeds_after_a_refused_one "${record_option[@]}"
sudo "$PREPARE" scenario exit

say "part 10: crashes"
# R02ag (decision 0056, amendment of 2026-11-13, crashes). Every test runs in a unit whose core limit
# is 1 MiB, so a missing guard cannot pass because the parent already had a limit of zero.
sudo "$PREPARE" counters-install
sudo "$PREPARE" listener-start
sudo "$PREPARE" routing-file
run_in_scenario "$lib_binary" crash_tests crash crash-crash job_crashes_leave_no_core -p LimitCORE=1M "${record_option[@]}"
run_in_scenario "$lib_binary" crash_tests crash-setter crash-setter job_setter_crashes_leave_no_core -p LimitCORE=1M "${record_option[@]}"
# Pipe routing: the launch is refused before any group exists, so the counter stays at 0; one trusted crash then moves it.
sudo "$PREPARE" routing-counter-pipe
run_in_scenario "$lib_binary" crash_tests crash crash-pipe job_pipe_routing_reaches_no_collector -p LimitCORE=1M "${record_option[@]}"
trusted_crash pipe
sudo "$PREPARE" routing-file
sudo "$PREPARE" routing-counter-socket
run_in_scenario "$lib_binary" crash_tests crash crash-socket job_socket_routing_reaches_no_collector -p LimitCORE=1M "${record_option[@]}"
trusted_crash socket
sudo "$PREPARE" routing-file
sudo "$PREPARE" routing-absolute
run_in_scenario "$lib_binary" crash_tests exit crash-absolute job_absolute_routing_is_refused -p LimitCORE=1M "${record_option[@]}"
sudo "$PREPARE" routing-file
# A change during an attempt: the test's linger attempt runs while this script sets the pipe routing.
routing_change crash-change "$OUT/routing-change"
sudo "$PREPARE" listener-stop
# Rule 13: the environment record must hold both counters before and after the trusted crash.
for key in core.pipe.before core.pipe.after core.socket.before core.socket.after; do
  grep -q "^${key}=" "$OUT/environment.txt" || fail "the environment record holds no $key"
done
sudo "$PREPARE" scenario exit

say "part 11: the guardian"
# R02ah (decision 0056, amendment of 2026-11-13, the guardian): renewals against the stand-in store, each test in one process.
run_in_scenario "$lib_binary" guardian_tests linger guardian-lease job_lease_loss_ends_the_attempt
run_in_scenario "$lib_binary" guardian_tests linger guardian-unavailable job_an_unavailable_store_lets_the_guardian_end_it
run_in_scenario "$lib_binary" guardian_tests linger guardian-readiness job_a_stop_at_readiness_releases_no_payload
run_in_scenario "$lib_binary" guardian_tests linger guardian-stall job_a_stalled_supervisor_is_ended_by_the_guardian
run_in_scenario "$lib_binary" guardian_tests hang guardian-faulty job_a_faulty_supervisor_is_ended_at_the_deadline
run_in_scenario "$lib_binary" guardian_tests hang guardian-confirmed job_confirmed_renewals_end_at_the_deadline
run_in_scenario "$lib_binary" guardian_tests copy guardian-fixture job_the_fixture_plan_still_yields_its_output
# The death of the supervisor at each phase, in a unit with KillMode=process (amendment rule 9).
death_at guarded 1 exit
death_at started 1 exit
death_at ready 1 exit
death_at running 1 linger
death_at collecting 1 copy
# Without the guardian, only the launcher's --die-with-parent ends the payload.
death_at running 0 linger
sudo "$PREPARE" scenario exit

say "part 12: the attempt registry"
# R02ai (decision 0056, amendment of 2026-11-14): the death of the supervisor at registered and created,
# then reconciliation alone in a second unit with the same execution directory.
for phase in registered created; do
  dir="$OUT/registry-$phase"
  rm -rf -- "$dir"
  mkdir -m 700 -- "$dir"
  death_at "$phase" 0 exit "$dir"
  sudo "$PREPARE" scenario exit
  run_unit "$lib_binary" "registry-reconcile-$phase" execution::attempt::registry_tests::job_reconcile_after_death -p Delegate=yes \
    --setenv="ARTIFREE_RECONCILE_DIR=$PWD/$dir" --setenv="ARTIFREE_HOLD_PHASE=$phase"
  # A staged input is read-only, so it is made writable before the removal.
  chmod -R u+w -- "$dir"
  rm -rf -- "$dir"
done
run_in_scenario "$lib_binary" registry_tests exit registry-same-boot job_a_same_boot_leftover_is_killed
run_in_scenario "$lib_binary" registry_tests exit registry-changed-boot job_a_changed_boot_leaves_its_group_alone
# Each malformed or foreign record quarantines its process, so each case runs in its own unit.
for case in group-name dotdot machine namespace stray; do
  run_in_scenario "$lib_binary" registry_tests exit "registry-bad-$case" job_bad_records_quarantine \
    --setenv="ARTIFREE_BAD_RECORD=$case"
done
# A parent owned by root holds a decoy that the record names; it must stay populated and alive.
decoy_group=attempt-72623859790382856-651345242494996240
sudo "$PREPARE" decoy-on "$decoy_group"
run_in_scenario "$lib_binary" registry_tests exit registry-root-parent job_a_root_owned_parent_quarantines \
  --setenv="ARTIFREE_DECOY_GROUP=$decoy_group"
decoy="$(sudo "$PREPARE" decoy-check "$decoy_group")"
printf '%s\n' "$decoy"
[[ $decoy == 'decoy populated=1 alive=1' ]] || fail "the root-owned decoy was touched: $decoy"
sudo "$PREPARE" decoy-off
# The persisted quarantine: the routing change of part 10 in a kept directory, then a second unit.
persist="$OUT/persist"
rm -rf -- "$persist"
mkdir -m 700 -- "$persist"
sudo "$PREPARE" routing-file
routing_change registry-routing-change "$OUT/routing-change-persist" --setenv="ARTIFREE_PERSIST_DIR=$PWD/$persist"
sudo "$PREPARE" scenario exit
run_unit "$lib_binary" registry-persisted execution::attempt::registry_tests::job_quarantine_is_persisted -p Delegate=yes \
  --setenv="ARTIFREE_PERSIST_DIR=$PWD/$persist"
chmod -R u+w -- "$persist"
rm -rf -- "$persist"
sudo "$PREPARE" scenario exit

say "part 13: the worker of run_profile jobs (R02bb, decision 0057 rules 2, 4 and 15 to 19)"
sudo "$PREPARE" routing-file
# The worker as the unit's own command (0057 rule 22): a fixture job is prepared outside the unit,
# run by worker run --execution-dir --once, then checked through the store.
unit_data="$OUT/server-unit-data"
unit_exec="$OUT/server-unit-exec"
rm -rf -- "$unit_data" "$unit_exec"
mkdir -m 700 -- "$unit_data" "$unit_exec"
sudo "$PREPARE" scenario copy
ARTIFREE_WORKER_DATA="$PWD/$unit_data" run_direct "$server_test" server-unit-prepare job_prepare_a_worker_unit
sudo systemd-run --quiet --wait --pipe --collect --service-type=exec \
  --uid="$(id -u)" --gid="$(id -g)" --working-directory="$PWD" -p Delegate=yes \
  -- "$server_bin" worker run --data-dir "$PWD/$unit_data" --execution-dir "$PWD/$unit_exec" --once \
  > "$OUT/server-unit.jsonl" 2> "$OUT/server-unit.stderr" \
  || fail "the worker unit failed; see $OUT/server-unit.stderr"
grep -Fq '"state":"succeeded"' "$OUT/server-unit.jsonl" || fail "the worker unit recorded no succeeded job"
ARTIFREE_WORKER_DATA="$PWD/$unit_data" run_direct "$server_test" server-unit-check job_check_the_worker_unit_result
chmod -R u+w -- "$unit_exec"
rm -rf -- "$unit_data" "$unit_exec"
run_direct "$server_test" server-undelegated job_a_worker_outside_a_delegated_group_refuses_to_start
run_server_job hang server-cancel job_cancel_ends_the_job_cancelled_after_cleanup
run_server_job survivors-hang server-cancel-descendant job_a_sigterm_ignoring_descendant_is_gone_at_cancel
run_server_job hang server-lease-loss job_lease_loss_schedules_a_retry
run_server_job linger server-trash-during-run job_a_trash_during_the_run_is_never_published
for part in fence cancel generation authority; do
  run_server_job hang "server-renewal-$part" job_a_renewal_stops_at_the_store_answer --setenv="ARTIFREE_RENEWAL_PART=$part"
done
run_server_job hang server-store-unavailable job_an_unavailable_store_ends_the_attempt_by_the_guardian
run_server_job hang server-stalled-worker job_a_stalled_worker_ends_the_attempt_by_the_guardian
run_server_job exit server-busy-lock job_a_busy_lock_spends_no_attempt
# The verdicts of R03bb (decision 0058 rule 18): each failed run has its own word on the worker's
# stderr, with the length of the payload's stderr that the provider handed over.
for case in fail thread-refusal hang memory flood; do
  run_server_job "$case" "server-verdict-$case" job_a_failed_run_reports_its_own_verdict --setenv="ARTIFREE_VERDICT_CASE=$case"
done
sudo "$PREPARE" scenario exit

# Part 14 is the recovery matrix of R02bc (decision 0057 rule 20, the R02bc row; 0042 rule 8). Each
# case is one ignored test of execution_jobs in a delegated unit, with its own scenario and label.
# run_server_job checks that no payload, probe or entry point outlived each case. The kill phases
# are the words of the provider's Phase (execution/attempt.rs) and the worker's `settled` pause;
# the record's removal before settlement (at collecting) is the separate server-kill-removed case.
say "part 14: the recovery matrix of R02bc (decision 0057 rule 20; 0042 rule 8)"
for phase in registered created guarded started ready running settled; do
  run_server_job copy "server-kill-$phase" recovery_kill::job_a_supervisor_killed_at_a_phase_is_cleaned_before_reclaim --setenv="ARTIFREE_KILL_PHASE=$phase"
done
run_server_job copy server-kill-removed recovery_kill::job_a_kill_between_record_removal_and_settlement_is_settled_without_a_record
run_server_job copy server-restored-fresh recovery_restore::job_a_restored_database_beside_a_fresh_directory_settles_nothing_and_quarantines
run_server_job hang server-stuck-cleanup recovery_restore::job_a_stuck_cleanup_keeps_the_quarantine
run_server_job survivors-hang server-purge-race recovery_races::job_purge_racing_a_sigterm_ignoring_descendant_keeps_its_tombstone
run_server_job copy server-trash-claim recovery_races::job_trash_and_recovery_never_publish_the_old_attempt --setenv=ARTIFREE_TRASH_AT=claim
run_server_job hang server-trash-heartbeat recovery_races::job_trash_and_recovery_never_publish_the_old_attempt --setenv=ARTIFREE_TRASH_AT=heartbeat
run_server_job copy server-trash-publication recovery_races::job_trash_and_recovery_never_publish_the_old_attempt --setenv=ARTIFREE_TRASH_AT=publication
run_server_job hang server-disable-heartbeat recovery_disable::job_disable_and_enable_before_a_heartbeat_stops_the_attempt
run_server_job copy server-disable-publication recovery_disable::job_disable_and_enable_before_publication_cancels_the_job
for across in restart restore; do
  run_server_job copy "server-disable-$across" recovery_disable::job_disable_and_enable_is_not_forgotten_across_a_restart_or_restore --setenv="ARTIFREE_ACROSS=$across"
done
run_server_job copy server-two-workers recovery_workers::job_two_workers_never_run_an_old_and_a_new_attempt_together
run_server_job copy server-resume recovery_workers::job_queued_and_retry_jobs_resume_after_trash_and_restart
sudo "$PREPARE" scenario exit

say "restore the routing and the profile"
sudo "$PREPARE" routing-pipe
sudo "$PREPARE" profile-off
say "enforcement checks completed"
