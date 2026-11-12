#!/usr/bin/env bash
# Operator's privileged preparation for the Linux enforcement job (decision 0056 rule 9).
# Runs only on the disposable hosted runner, under sudo. It is kept apart from the
# product's rootless requirements: the product never runs this script and asks for no
# privilege. The system-wide user-namespace restriction stays on; only the tool root's
# launcher, by its path, may create user namespaces.
#
# Usage: prepare.sh VERB [ARGUMENT]. Only run.sh calls it.
set -euo pipefail

# The tool root's fixed path (decision 0056 rule 6). It must equal ROOT in run.sh and
# the provider's TOOL_ROOT.
readonly ROOT=/opt/artifree/tools/v1
readonly SAVED_DIR=/run/artifree-enforcement
readonly SAVED_ROUTING="$SAVED_DIR/core_pattern"
readonly CORE_PATTERN=/proc/sys/kernel/core_pattern
readonly PROFILE=/etc/apparmor.d/artifree-enforcement-launcher
readonly SLICE=/run/systemd/system/artifree-enforcement-nomem.slice
readonly BUBBLEWRAP_VERSION=0.9.0-1ubuntu0.3

usage() {
  cat >&2 <<'EOF'
usage: prepare.sh VERB [ARGUMENT]
  install FIXTURE     install the pinned bubblewrap and assemble the tool root
  save-routing        save kernel.core_pattern once, before the job changes it
  routing-file        set kernel.core_pattern to the plain file name core
  routing-pipe        restore the saved kernel.core_pattern
  profile-on          load the AppArmor profile for the tool root's launcher
  profile-off         unload and remove that profile
  launcher-away       move the launcher aside (rule 10 removal)
  launcher-back       move the launcher back
  fixture-away        move the fixture aside (rule 10 removal)
  fixture-back        move the fixture back
  nomem-slice-on      load the slice that removes the memory controller
  nomem-slice-off     remove that slice
EOF
  exit 2
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

# Assembles the tool root: a copy of the launcher, the fixture, and the fixture's loader
# and shared libraries. The sandbox binds the tool root's lib and lib64 as /lib and
# /lib64 and nothing else, so a library that ldd reports under /usr/lib (usrmerged
# hosts such as ubuntu-24.04) is copied to the path it has inside the sandbox.
install_tool_root() {
  local fixture=$1 ldd_output lib dest
  [[ -f "$fixture" && -x "$fixture" ]] || fail "the fixture is not an executable file: $fixture"
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  apt-get install -y --no-install-recommends "bubblewrap=$BUBBLEWRAP_VERSION"
  rm -rf -- "$ROOT"
  install -d -o root -g root -m 0755 "$ROOT/bin" "$ROOT/lib" "$ROOT/lib64"
  # A copy, not a link: the AppArmor profile names the launcher by this path.
  install -o root -g root -m 0755 /usr/bin/bwrap "$ROOT/bin/bwrap"
  install -o root -g root -m 0755 "$fixture" "$ROOT/bin/artifree-execution-fixture"
  ldd_output="$(ldd "$fixture")"
  case "$ldd_output" in
    *"not found"*) fail "the fixture has a library that does not resolve" ;;
  esac
  while IFS= read -r lib; do
    case "$lib" in
      /lib/* | /lib64/*) dest="$ROOT$lib" ;;
      /usr/lib/*) dest="$ROOT/lib/${lib#/usr/lib/}" ;;
      /usr/lib64/*) dest="$ROOT/lib64/${lib#/usr/lib64/}" ;;
      *) fail "the fixture needs a library outside /lib, /lib64, /usr/lib and /usr/lib64: $lib" ;;
    esac
    install -D -o root -g root -m 0755 "$(readlink -f -- "$lib")" "$dest"
  done < <(printf '%s\n' "$ldd_output" | awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^\//) { print $i; break } }')
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
  sysctl -w kernel.core_pattern=core
  [[ "$(cat -- "$CORE_PATTERN")" == core ]] || fail "kernel.core_pattern does not read back as core"
}

routing_pipe() {
  [[ -f "$SAVED_ROUTING" ]] || fail "no saved core_pattern at $SAVED_ROUTING; run save-routing first"
  cat -- "$SAVED_ROUTING" > "$CORE_PATTERN"
  cmp -s -- "$SAVED_ROUTING" "$CORE_PATTERN" || fail "kernel.core_pattern does not match the saved value"
}

profile_on() {
  cat > "$PROFILE" <<EOF
abi <abi/4.0>,
include <tunables/global>
profile artifree-enforcement-launcher $ROOT/bin/bwrap flags=(unconfined) {
  userns,
}
EOF
  apparmor_parser -r "$PROFILE"
}

profile_off() {
  apparmor_parser -R "$PROFILE"
  rm -- "$PROFILE"
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

nomem_slice_on() {
  printf '[Slice]\nDisableControllers=memory\n' > "$SLICE"
  systemctl daemon-reload
}

nomem_slice_off() {
  rm -- "$SLICE"
  systemctl daemon-reload
}

main() {
  local verb=${1:-}
  case "$verb" in
    install) (( $# == 2 )) || usage ;;
    save-routing | routing-file | routing-pipe | profile-on | profile-off | \
      launcher-away | launcher-back | fixture-away | fixture-back | \
      nomem-slice-on | nomem-slice-off) (( $# == 1 )) || usage ;;
    *) usage ;;
  esac
  if (( EUID != 0 )); then
    fail "run as root through sudo"
  fi
  case "$verb" in
    install) install_tool_root "$2" ;;
    save-routing) save_routing ;;
    routing-file) routing_file ;;
    routing-pipe) routing_pipe ;;
    profile-on) profile_on ;;
    profile-off) profile_off ;;
    launcher-away) move_away "$ROOT/bin/bwrap" ;;
    launcher-back) move_back "$ROOT/bin/bwrap" ;;
    fixture-away) move_away "$ROOT/bin/artifree-execution-fixture" ;;
    fixture-back) move_back "$ROOT/bin/artifree-execution-fixture" ;;
    nomem-slice-on) nomem_slice_on ;;
    nomem-slice-off) nomem_slice_off ;;
  esac
}

main "$@"
