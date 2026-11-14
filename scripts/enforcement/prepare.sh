#!/usr/bin/env bash
# Operator's privileged preparation for the Linux enforcement job (decision 0056 rule 9).
# Runs only on the disposable hosted runner, under sudo. It is kept apart from the
# product's rootless requirements: the product never runs this script and asks for no
# privilege. The system-wide user-namespace restriction stays on; only the tool root's
# launcher, by its path, may create user namespaces.
#
# Usage: prepare.sh VERB [ARGUMENT]. Only run.sh calls it.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
# VERSION, ROOT and the operator's packaging functions (decision 0058 rule 19): the job builds
# its root with the same functions, then adds its test programs. run.sh sources the same file.
# shellcheck source=version.sh
source "$SCRIPT_DIR/version.sh"
readonly SAVED_DIR=/run/artifree-enforcement
readonly SAVED_ROUTING="$SAVED_DIR/core_pattern"
readonly CORE_PATTERN=/proc/sys/kernel/core_pattern
readonly SLICE=/run/systemd/system/artifree-enforcement-nomem.slice
# The counters are root-owned and outside /tmp, which only root can write (decision 0056,
# amendment of 2026-11-13, crashes).
readonly COUNTERS_DIR=/var/lib/artifree-enforcement
readonly COLLECTOR_DIR=/opt/artifree-enforcement
readonly COLLECTOR="$COLLECTOR_DIR/collector"
# The listener's socket lives in the saved directory, which is root-only (0700).
readonly SOCKET_DIR="$SAVED_DIR"
readonly SOCKET="$SOCKET_DIR/core.sock"
readonly LISTENER_UNIT=artifree-core-listener
# The root-owned parent of the decoy groups and the file that keeps the decoy's pid in the saved
# directory (decision 0056, amendment of 2026-11-14, the attempt registry).
readonly DECOY_ROOT=/sys/fs/cgroup/artifree-decoy
readonly DECOY_PID="$SAVED_DIR/decoy.pid"

usage() {
  cat >&2 <<'EOF'
usage: prepare.sh VERB [ARGUMENT]
  install FIXTURE PAYLOAD SERVER  assemble the tool root with the packaging script, refusing an existing one
  save-routing        save kernel.core_pattern once, before the job changes it
  routing-file        set kernel.core_pattern to the plain file name core
  routing-pipe        restore the saved kernel.core_pattern
  counters-install    install the root-owned collector copy and empty pipe and socket counters
  listener-start      start the core listener unit and wait up to 5 s for its socket
  listener-stop       stop the core listener unit and remove its socket; never fails if absent
  routing-counter-pipe    set kernel.core_pattern to the counting collector pipe
  routing-counter-socket  set kernel.core_pattern to the listener's socket
  routing-absolute    set kernel.core_pattern to /var/tmp/core
  counters-show       print core.pipe=<count> and core.socket=<count>
  profile-on          load the AppArmor profile for the tool root's launcher
  profile-off         unload and remove that profile
  launcher-away       move the launcher aside (rule 10 removal)
  launcher-back       move the launcher back
  fixture-away        move the fixture aside (rule 10 removal)
  fixture-back        move the fixture back
  nomem-slice-on      load the slice that removes the memory controller
  nomem-slice-off     remove that slice
  tool-modify         append one byte to the payload, keeping a copy of the original
  tool-restore        restore the original payload
  tool-extra-on       add a file that the manifest does not list
  tool-extra-off      remove that file
  tool-owner-on       give the bubblewrap licence to nobody
  tool-owner-off      give it back to root
  tool-writable-on    make the tool root's etc directory group-writable
  tool-writable-off   make it writable by root only again
  tool-setid-on       set the set-user-ID bit of the payload
  tool-setid-off      clear it
  ancestor-open-on    make the tools directory, the root's parent, writable by others
  ancestor-open-off   make it writable by root only again
  tool-missing-on     move the bubblewrap licence out of the tool root
  tool-missing-off    move that licence back
  manifest-away       move MANIFEST out of the tool root
  manifest-back       move MANIFEST back
  payload-break       replace the payload by a text file and rewrite the manifest
  payload-restore     restore the payload and rewrite the manifest
  scenario NAME       write NAME to etc/scenario and rewrite the manifest
                      (exit threads processes memory scratch-bytes scratch-inodes
                      flood hang cpu survivors survivors-hang probes clone3
                      thread-refusal descendant-refusal lower descriptors watch
                      network host proc-dev environment marker
                      copy input-write out-socket out-many out-big-file out-big-total
                      out-bomb out-deep out-invalid-name out-collision out-extension
                      out-no-entry out-fail out-plain
                      crash crash-setter linger fail)
  kernel-log          print the last 20 kernel log lines about seccomp (type=1326) or oom
  kernel-log-access   let the runner read the kernel log and lift its rate limit; refuse while an audit daemon runs
  supervisor-clear UNIT  remove the empty attempt groups directly beneath the supervisor test unit UNIT
                      (artifree-supervisor-<phase>-<0|1>, where phase is registered, created, guarded,
                      started, ready, running or collecting); a group that is not empty stays and fails the verb
  decoy-on NAME     make the root-owned group artifree-decoy/NAME (attempt-<digits>-<digits>) holding a trusted sleep
  decoy-check NAME  print decoy populated=<0|1> alive=<0|1>
  decoy-off         kill and remove every decoy group and artifree-decoy; never fails if absent
EOF
  exit 2
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

# Writes the runner's primary IPv4 address to etc/host-address, one line, for the network
# scenario. Decision 0056 (amendment of 2026-11-13, "files, descriptors and network"): "The
# runner's own address is a fact of the prepared host: the preparation writes it into the tool
# root, under the manifest, as it writes the scenario." The address is the source address of
# the route to 1.1.1.1.
write_host_address() {
  local route address
  route="$(ip -4 -o route get 1.1.1.1)" \
    || fail "ip names no route to 1.1.1.1, so the runner's own IPv4 address is unknown"
  address="$(printf '%s\n' "$route" |
    awk '{ for (i = 1; i < NF; i++) if ($i == "src") { print $(i + 1); exit } }')"
  [[ "$address" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]] \
    || fail "the route to 1.1.1.1 names no IPv4 source address: $route"
  write_root_text 0644 "$ROOT/etc/host-address" "$address"
}

# Assembles the tool root with the packaging script's functions (decision 0058 rule 19): the
# bubblewrap launcher and the server binary as the script installs them, then the job's fixture
# and payload, the shared libraries each of them needs, the licence of each Debian package that
# owns a copied file, the scenario file that starts as `exit`, the runner's IPv4 address in
# etc/host-address, and the manifest last (decision 0056 rule 6). Like the script, it refuses a
# root that exists. The fixture's write-probes mode writes the probe programs under probes/ before the manifest, which covers
# them like every other file: "The fixture's tool root also holds its generated probe programs,
# under the same manifest" (decision 0056 rule 6, as amended 2026-11-13).
# The server binary serves the worker's stub, bootstrap and guardian entry points (decision 0057
# rule 4). It is installed stripped, because every admission hashes every file of the tool root.
# The fixture stays for admission's namespace check and probe child, the probe programs and the
# counters.
install_tool_root() {
  local fixture=$1 payload=$2 server=$3 named
  [[ -f "$fixture" && -x "$fixture" ]] || fail "the fixture is not an executable file: $fixture"
  [[ -f "$payload" && -x "$payload" ]] || fail "the payload is not an executable file: $payload"
  named="$(server_version "$server")"
  [[ "$named" == "$VERSION" ]] || fail "the server names version $named, the workspace $VERSION"
  begin_root
  install_launcher
  install -o root -g root -m 0755 "$fixture" "$ROOT/bin/artifree-execution-fixture"
  copy_libraries "$fixture"
  install -o root -g root -m 0755 "$payload" "$ROOT/bin/artifree-execution-payload"
  copy_libraries "$payload"
  install_server "$server"
  write_root_text 0644 "$ROOT/etc/scenario" exit
  write_host_address
  "$ROOT/bin/artifree-execution-fixture" write-probes "$ROOT/probes"
  chown -R root:root "$ROOT/probes"
  chmod -R 0755 "$ROOT/probes"
  write_manifest
}

# The directory of saved copies and of the saved routing value. Only root may read it.
ensure_saved_dir() {
  install -d -o root -g root -m 0700 "$SAVED_DIR"
}

save_routing() {
  install -d -o root -g root -m 0700 "$SAVED_DIR"
  if [[ -e "$SAVED_ROUTING" ]]; then
    printf 'kept the saved core_pattern at %s\n' "$SAVED_ROUTING" >&2
    return 0
  fi
  (umask 077 && cat -- "$CORE_PATTERN" > "$SAVED_ROUTING")
}

routing_file() {
  set_routing core
}

routing_pipe() {
  [[ -f "$SAVED_ROUTING" ]] || fail "no saved core_pattern at $SAVED_ROUTING; run save-routing first"
  cat -- "$SAVED_ROUTING" > "$CORE_PATTERN"
  # Compared as text: a proc file reports size 0, and cmp calls two regular files of
  # different sizes different without reading them.
  [[ "$(cat -- "$CORE_PATTERN")" == "$(cat -- "$SAVED_ROUTING")" ]] \
    || fail "kernel.core_pattern does not match the saved value"
}

# Decision 0056 rule 9 (as reworded) and its amendment of 2026-11-13 (crashes): writes VALUE to
# kernel.core_pattern and requires it to read back.
set_routing() {
  local value=$1
  sysctl -w "kernel.core_pattern=$value" > /dev/null
  [[ "$(cat -- "$CORE_PATTERN")" == "$value" ]] \
    || fail "kernel.core_pattern does not read back as $value"
}

# Decision 0056 rule 9 (as reworded) and its amendment of 2026-11-13 (crashes): creates the
# root-owned collector copy and the two counters, emptied. The copy lives outside the tool root,
# so the manifest does not cover it.
counters_install() {
  install -d -o root -g root -m 0755 "$COUNTERS_DIR" "$COLLECTOR_DIR"
  install -o root -g root -m 0755 "$ROOT/bin/artifree-execution-fixture" "$COLLECTOR"
  install -o root -g root -m 0644 /dev/null "$COUNTERS_DIR/pipe.count"
  install -o root -g root -m 0644 /dev/null "$COUNTERS_DIR/socket.count"
}

# Decision 0056 rule 9 (as reworded) and its amendment of 2026-11-13 (crashes): starts the socket
# listener as a transient unit and waits for its socket. An existing path is refused first: the
# listener refuses it too, and a stale socket would satisfy the wait.
listener_start() {
  ensure_saved_dir
  [[ -x "$COLLECTOR" ]] || fail "no collector at $COLLECTOR; run counters-install first"
  [[ ! -e "$SOCKET" && ! -L "$SOCKET" ]] || fail "$SOCKET already exists"
  systemd-run --quiet --unit="$LISTENER_UNIT" --collect \
    "$COLLECTOR" core-listener "$SOCKET" "$COUNTERS_DIR/socket.count"
  local tries
  for ((tries = 0; tries < 50; tries++)); do
    [[ -S "$SOCKET" ]] && return 0
    sleep 0.1
  done
  fail "the core listener did not create $SOCKET within 5 s"
}

# Decision 0056 rule 9 (as reworded) and its amendment of 2026-11-13 (crashes): stops the listener
# if it runs and removes its socket. Both steps tolerate absence.
listener_stop() {
  if systemctl is-active --quiet "$LISTENER_UNIT"; then
    systemctl stop "$LISTENER_UNIT"
  fi
  rm -f -- "$SOCKET"
}

# Decision 0056 rule 9 (as reworded) and its amendment of 2026-11-13 (crashes): points
# kernel.core_pattern at the counting collector, so that each dump adds a line to pipe.count.
routing_counter_pipe() {
  [[ -x "$COLLECTOR" ]] || fail "no collector at $COLLECTOR; run counters-install first"
  set_routing "|$COLLECTOR core-collector $COUNTERS_DIR/pipe.count"
}

# Decision 0056 rule 9 (as reworded) and its amendment of 2026-11-13 (crashes): points
# kernel.core_pattern at the listener's socket, so that each dump adds a line to socket.count.
routing_counter_socket() {
  [[ -S "$SOCKET" ]] || fail "no listener socket at $SOCKET; run listener-start first"
  set_routing "@$SOCKET"
}

# Decision 0056 rule 9 (as reworded) and its amendment of 2026-11-13 (crashes): points
# kernel.core_pattern at an absolute path, which the provider must refuse.
routing_absolute() {
  set_routing /var/tmp/core
}

# Decision 0056 rule 9 (as reworded) and its amendment of 2026-11-13 (crashes): prints the line
# counts of the two counters, which run.sh records.
counters_show() {
  local pipe socket
  pipe="$(wc -l < "$COUNTERS_DIR/pipe.count")"
  socket="$(wc -l < "$COUNTERS_DIR/socket.count")"
  printf 'core.pipe=%s\ncore.socket=%s\n' "$pipe" "$socket"
}

profile_on() {
  load_launcher_profile "$VERSION"
}

profile_off() {
  unload_launcher_profile "$VERSION"
}

# Moves a tool root file aside for one rule 10 removal, and back. Never overwrites.
move_away() {
  local path=$1 aside="$1.away"
  [[ -e "$path" ]] || fail "$path is missing"
  [[ ! -e "$aside" ]] || fail "$aside already exists"
  mv -- "$path" "$aside"
}

move_back() {
  local path=$1 aside="$1.away"
  [[ -e "$aside" ]] || fail "$aside is missing"
  [[ ! -e "$path" ]] || fail "$path already exists"
  mv -- "$aside" "$path"
}

# Moves a tool root file into SAVED_DIR for one removal, and back. Never overwrites.
move_to_saved() {
  local path=$1 saved=$2
  ensure_saved_dir
  [[ -e "$path" ]] || fail "$path is missing"
  [[ ! -e "$saved" ]] || fail "$saved already exists"
  mv -- "$path" "$saved"
}

move_from_saved() {
  local path=$1 saved=$2
  ensure_saved_dir
  [[ -e "$saved" ]] || fail "$saved is missing"
  [[ ! -e "$path" ]] || fail "$path already exists"
  mv -- "$saved" "$path"
}

# Appends one byte to the payload and keeps a copy of the original. The manifest is not
# rewritten, so the provider must refuse the tool root.
tool_modify() {
  local payload="$ROOT/bin/artifree-execution-payload" saved="$SAVED_DIR/payload.original"
  ensure_saved_dir
  [[ -f "$payload" ]] || fail "$payload is missing"
  [[ ! -e "$saved" ]] || fail "$saved already exists"
  cp -p -- "$payload" "$saved"
  printf 'x' >> "$payload"
}

tool_restore() {
  local payload="$ROOT/bin/artifree-execution-payload" saved="$SAVED_DIR/payload.original"
  ensure_saved_dir
  [[ -e "$saved" ]] || fail "$saved is missing"
  install -o root -g root -m 0755 "$saved" "$payload"
  rm -- "$saved"
}

tool_extra_on() {
  local extra="$ROOT/bin/extra"
  ensure_saved_dir
  [[ ! -e "$extra" ]] || fail "$extra already exists"
  write_root_text 0644 "$extra" extra
}

tool_extra_off() {
  local extra="$ROOT/bin/extra"
  ensure_saved_dir
  [[ -e "$extra" ]] || fail "$extra is missing"
  rm -- "$extra"
}

tool_missing_on() {
  move_to_saved "$ROOT/licenses/bubblewrap.copyright" "$SAVED_DIR/bubblewrap.copyright"
}

tool_missing_off() {
  move_from_saved "$ROOT/licenses/bubblewrap.copyright" "$SAVED_DIR/bubblewrap.copyright"
}

# Replaces the payload by a text file, so that only the exec of the payload can fail. The
# manifest is rewritten to match the replacement.
payload_break() {
  local payload="$ROOT/bin/artifree-execution-payload" saved="$SAVED_DIR/payload.unbroken"
  ensure_saved_dir
  [[ -f "$payload" ]] || fail "$payload is missing"
  [[ ! -e "$saved" ]] || fail "$saved already exists"
  cp -p -- "$payload" "$saved"
  write_root_text 0755 "$payload" 'not a program'
  write_manifest
}

payload_restore() {
  local payload="$ROOT/bin/artifree-execution-payload" saved="$SAVED_DIR/payload.unbroken"
  ensure_saved_dir
  [[ -e "$saved" ]] || fail "$saved is missing"
  install -o root -g root -m 0755 "$saved" "$payload"
  write_manifest
  rm -- "$saved"
}

nomem_slice_on() {
  printf '[Slice]\nDisableControllers=memory\n' > "$SLICE"
  systemctl daemon-reload
}

nomem_slice_off() {
  rm -- "$SLICE"
  systemctl daemon-reload
}

is_scenario() {
  case "$1" in
    exit | threads | processes | memory | scratch-bytes | scratch-inodes | flood | hang | \
      cpu | survivors | survivors-hang | probes | clone3 | thread-refusal | descendant-refusal | \
      lower | descriptors | watch | network | host | proc-dev | environment | marker) return 0 ;;
    copy | input-write | out-socket | out-many | out-big-file | out-big-total | out-bomb | out-deep | \
      out-invalid-name | out-collision | out-extension | out-no-entry | out-fail | out-plain) return 0 ;;
    crash | crash-setter | linger | fail) return 0 ;;
    *) return 1 ;;
  esac
}

# Writes NAME to the scenario file the payload reads, then rewrites the manifest so that the
# provider still admits the tool root (decision 0056 rule 6). The name is checked by the caller.
set_scenario() {
  local name=$1
  [[ -d "$ROOT/etc" ]] || fail "the tool root has no etc directory; run install first"
  write_root_text 0644 "$ROOT/etc/scenario" "$name"
  write_manifest
}

# Prints the last 20 kernel log lines about seccomp violations (audit type 1326) or the OOM
# killer. It changes nothing and is for diagnosis only.
kernel_log() {
  dmesg | { grep -E 'type=1326|oom' || true; } | tail -n 20
}

# Decision 0056 rule 9, part 3 as amended: this "lets the runner's account read the kernel log
# (`kernel.dmesg_restrict` 0) and lifts the kernel's message rate limit (`kernel.printk_ratelimit`
# 0), on the job's disposable runner only". Seccomp records reach the kernel log only while no
# audit daemon holds the kernel's audit channel, so a running daemon is refused by name. The verb
# prints one line, which run.sh records among the environment.
kernel_log_access() {
  local status=0 dmesg_restrict printk_ratelimit
  pgrep -x auditd > /dev/null || status=$?
  case "$status" in
    0) fail "an audit daemon (auditd) is running, so seccomp records do not reach the kernel log" ;;
    1) ;;
    *) fail "the audit daemon check failed with status $status" ;;
  esac
  if systemctl is-active --quiet auditd; then
    fail "the auditd service is active, so seccomp records do not reach the kernel log"
  fi
  sysctl -w kernel.dmesg_restrict=0 > /dev/null
  sysctl -w kernel.printk_ratelimit=0 > /dev/null
  dmesg_restrict="$(cat -- /proc/sys/kernel/dmesg_restrict)"
  printk_ratelimit="$(cat -- /proc/sys/kernel/printk_ratelimit)"
  [[ "$dmesg_restrict" == 0 ]] || fail "kernel.dmesg_restrict does not read back as 0"
  [[ "$printk_ratelimit" == 0 ]] || fail "kernel.printk_ratelimit does not read back as 0"
  printf 'kernel-log-access dmesg_restrict=%s printk_ratelimit=%s auditd=absent\n' \
    "$dmesg_restrict" "$printk_ratelimit"
}

# R02ah (decision 0056, amendment of 2026-11-13, the guardian): a supervisor killed at a phase
# leaves its empty attempt group, which only R02ai's reconciliation would remove. This removes the
# groups named attempt-<digits>-<digits> directly beneath the named test unit's group and nothing
# else. rmdir refuses a group that is not empty, and that refusal fails the verb.
supervisor_clear() {
  local unit=$1 group entry name
  group="/sys/fs/cgroup/system.slice/$unit.service"
  [[ -d "$group" ]] || return 0
  for entry in "$group"/attempt-*; do
    [[ -e "$entry" ]] || continue
    name="${entry##*/}"
    [[ $name =~ ^attempt-[0-9]+-[0-9]+$ && -d "$entry" && ! -L "$entry" ]] || continue
    rmdir -- "$entry"
  done
}

is_supervisor_unit() {
  [[ $1 =~ ^artifree-supervisor-(registered|created|guarded|started|ready|running|collecting)-[01]$ ]]
}

is_decoy_name() {
  [[ $1 =~ ^attempt-[0-9]+-[0-9]+$ ]]
}

# Decision 0056, amendment of 2026-11-14 (the attempt registry): a parent owned by root, made by the
# preparation script, holding the decoy. The decoy is a trusted sleep in a group named like an attempt
# group, so that a reconciliation which followed a record into it would find a live process. The
# script is not a process group leader, so setsid runs sleep in place and $! is sleep's own pid.
decoy_on() {
  local name=$1 pid
  ensure_saved_dir
  mkdir -p -- "$DECOY_ROOT"
  chmod 0755 -- "$DECOY_ROOT"
  mkdir -- "$DECOY_ROOT/$name"
  setsid /bin/sleep 600 < /dev/null > /dev/null 2>&1 &
  pid=$!
  printf '%s\n' "$pid" > "$DECOY_ROOT/$name/cgroup.procs"
  grep -qx -- "$pid" "$DECOY_ROOT/$name/cgroup.procs" || fail "the decoy process is not in its group"
  printf '%s\n' "$pid" > "$DECOY_PID"
}

# Returns 0 when the decoy group NAME holds no process or has no events file, and 1 when it does.
# An events file without a populated line fails the verb.
decoy_empty() {
  local events="$DECOY_ROOT/$1/cgroup.events" populated
  [[ -f "$events" ]] || return 0
  populated="$(awk '$1 == "populated" { print $2 }' "$events")"
  case "$populated" in
    0) return 0 ;;
    1) return 1 ;;
    *) fail "$events names no populated flag" ;;
  esac
}

# Prints decoy populated=<0|1> alive=<0|1> for the decoy group NAME: populated from its events file,
# alive from kill -0 of the pid that decoy-on saved.
decoy_check() {
  local name=$1 populated alive=0
  if decoy_empty "$name"; then
    populated=0
  else
    populated=1
  fi
  if [[ -f "$DECOY_PID" ]] && kill -0 "$(cat -- "$DECOY_PID")" 2> /dev/null; then
    alive=1
  fi
  printf 'decoy populated=%s alive=%s\n' "$populated" "$alive"
}

# Kills every decoy group, waiting up to 5 s for each to empty, then removes the groups, the root
# and the pid file. Absence of any of them is not a failure.
decoy_off() {
  local entry name tries
  if [[ ! -d "$DECOY_ROOT" ]]; then
    rm -f -- "$DECOY_PID"
    return 0
  fi
  for entry in "$DECOY_ROOT"/attempt-*; do
    [[ -e "$entry" ]] || continue
    name="${entry##*/}"
    [[ $name =~ ^attempt-[0-9]+-[0-9]+$ && -d "$entry" && ! -L "$entry" ]] || continue
    printf '1\n' > "$entry/cgroup.kill"
    for ((tries = 0; tries < 50; tries++)); do
      decoy_empty "$name" && break
      sleep 0.1
    done
    decoy_empty "$name" || fail "the decoy group $name still has processes after 5 s"
    rmdir -- "$entry"
  done
  rmdir -- "$DECOY_ROOT"
  rm -f -- "$DECOY_PID"
}

main() {
  local verb=${1:-}
  case "$verb" in
    install) (( $# == 4 )) || usage ;;
    scenario)
      if (( $# != 2 )) || ! is_scenario "$2"; then
        usage
      fi
      ;;
    supervisor-clear)
      if (( $# != 2 )) || ! is_supervisor_unit "$2"; then
        usage
      fi
      ;;
    decoy-on | decoy-check)
      if (( $# != 2 )) || ! is_decoy_name "$2"; then
        usage
      fi
      ;;
    save-routing | routing-file | routing-pipe | profile-on | profile-off | \
      launcher-away | launcher-back | fixture-away | fixture-back | \
      nomem-slice-on | nomem-slice-off | tool-modify | tool-restore | \
      tool-extra-on | tool-extra-off | tool-missing-on | tool-missing-off | \
      tool-owner-on | tool-owner-off | tool-writable-on | tool-writable-off | \
      tool-setid-on | tool-setid-off | ancestor-open-on | ancestor-open-off | \
      manifest-away | manifest-back | payload-break | payload-restore | \
      kernel-log | kernel-log-access) (( $# == 1 )) || usage ;;
    counters-install | listener-start | listener-stop | routing-counter-pipe | \
      routing-counter-socket | routing-absolute | counters-show) (( $# == 1 )) || usage ;;
    decoy-off) (( $# == 1 )) || usage ;;
    *) usage ;;
  esac
  if (( EUID != 0 )); then
    fail "run as root through sudo"
  fi
  case "$verb" in
    install) install_tool_root "$2" "$3" "$4" ;;
    save-routing) save_routing ;;
    routing-file) routing_file ;;
    routing-pipe) routing_pipe ;;
    counters-install) counters_install ;;
    listener-start) listener_start ;;
    listener-stop) listener_stop ;;
    routing-counter-pipe) routing_counter_pipe ;;
    routing-counter-socket) routing_counter_socket ;;
    routing-absolute) routing_absolute ;;
    counters-show) counters_show ;;
    profile-on) profile_on ;;
    profile-off) profile_off ;;
    launcher-away) move_away "$ROOT/bin/bwrap" ;;
    launcher-back) move_back "$ROOT/bin/bwrap" ;;
    fixture-away) move_away "$ROOT/bin/artifree-execution-fixture" ;;
    fixture-back) move_back "$ROOT/bin/artifree-execution-fixture" ;;
    nomem-slice-on) nomem_slice_on ;;
    nomem-slice-off) nomem_slice_off ;;
    tool-modify) tool_modify ;;
    tool-restore) tool_restore ;;
    tool-extra-on) tool_extra_on ;;
    tool-extra-off) tool_extra_off ;;
    tool-owner-on) chown nobody -- "$ROOT/licenses/bubblewrap.copyright" ;;
    tool-owner-off) chown root -- "$ROOT/licenses/bubblewrap.copyright" ;;
    tool-writable-on) chmod g+w -- "$ROOT/etc" ;;
    tool-writable-off) chmod g-w -- "$ROOT/etc" ;;
    tool-setid-on) chmod u+s -- "$ROOT/bin/artifree-execution-payload" ;;
    tool-setid-off) chmod u-s -- "$ROOT/bin/artifree-execution-payload" ;;
    ancestor-open-on) chmod o+w -- "$TOOLS_DIR" ;;
    ancestor-open-off) chmod o-w -- "$TOOLS_DIR" ;;
    tool-missing-on) tool_missing_on ;;
    tool-missing-off) tool_missing_off ;;
    manifest-away) move_to_saved "$ROOT/MANIFEST" "$SAVED_DIR/MANIFEST" ;;
    manifest-back) move_from_saved "$ROOT/MANIFEST" "$SAVED_DIR/MANIFEST" ;;
    payload-break) payload_break ;;
    payload-restore) payload_restore ;;
    scenario) set_scenario "$2" ;;
    kernel-log) kernel_log ;;
    kernel-log-access) kernel_log_access ;;
    supervisor-clear) supervisor_clear "$2" ;;
    decoy-on) decoy_on "$2" ;;
    decoy-check) decoy_check "$2" ;;
    decoy-off) decoy_off ;;
  esac
}

main "$@"
