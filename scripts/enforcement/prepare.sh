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
  install FIXTURE PAYLOAD  install the pinned bubblewrap and assemble the tool root
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
  tool-modify         append one byte to the payload, keeping a copy of the original
  tool-restore        restore the original payload
  tool-extra-on       add a file that the manifest does not list
  tool-extra-off      remove that file
  tool-missing-on     move the bubblewrap licence out of the tool root
  tool-missing-off    move that licence back
  manifest-away       move MANIFEST out of the tool root
  manifest-back       move MANIFEST back
  payload-break       replace the payload by a text file and rewrite the manifest
  payload-restore     restore the payload and rewrite the manifest
EOF
  exit 2
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

# Assembles the tool root: the bubblewrap launcher, the fixture and the payload, the shared
# libraries each of them needs, the licence of each Debian package that owns a copied file,
# and the manifest last (decision 0056 rule 6). The sandbox binds the tool root's lib and
# lib64 as /lib and /lib64 and nothing else, so a library that ldd reports under /usr/lib
# (usrmerged hosts such as ubuntu-24.04) is copied to the path it has inside the sandbox.
install_tool_root() {
  local fixture=$1 payload=$2
  [[ -f "$fixture" && -x "$fixture" ]] || fail "the fixture is not an executable file: $fixture"
  [[ -f "$payload" && -x "$payload" ]] || fail "the payload is not an executable file: $payload"
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  apt-get install -y --no-install-recommends "bubblewrap=$BUBBLEWRAP_VERSION"
  rm -rf -- "$ROOT"
  install -d -o root -g root -m 0755 "$ROOT/bin" "$ROOT/lib" "$ROOT/lib64" "$ROOT/licenses"
  # A copy, not a link: the AppArmor profile names the launcher by this path.
  install -o root -g root -m 0755 /usr/bin/bwrap "$ROOT/bin/bwrap"
  install_licence /usr/bin/bwrap
  install -o root -g root -m 0755 "$fixture" "$ROOT/bin/artifree-execution-fixture"
  copy_libraries "$fixture"
  install -o root -g root -m 0755 "$payload" "$ROOT/bin/artifree-execution-payload"
  copy_libraries "$payload"
  write_manifest
}

# Copies the shared libraries BINARY needs to the paths the sandbox gives them, and installs
# the licence of the Debian package that owns each copied library.
copy_libraries() {
  local binary=$1 ldd_output lib dest
  ldd_output="$(ldd "$binary")"
  case "$ldd_output" in
    *"not found"*) fail "$binary has a library that does not resolve" ;;
  esac
  while IFS= read -r lib; do
    case "$lib" in
      /lib/* | /lib64/*) dest="$ROOT$lib" ;;
      /usr/lib/*) dest="$ROOT/lib/${lib#/usr/lib/}" ;;
      /usr/lib64/*) dest="$ROOT/lib64/${lib#/usr/lib64/}" ;;
      *) fail "$binary needs a library outside /lib, /lib64, /usr/lib and /usr/lib64: $lib" ;;
    esac
    install -D -o root -g root -m 0755 "$(readlink -f -- "$lib")" "$dest"
    install_licence "$lib"
  done < <(printf '%s\n' "$ldd_output" | awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^\//) { print $i; break } }')
}

# The /lib <-> /usr/lib alias of PATH, or nothing for other paths. A usrmerged host lists a
# package's files under /usr/lib while ldd reports the same file under /lib.
usr_lib_alias() {
  case "$1" in
    /usr/lib/*) printf '/lib/%s\n' "${1#/usr/lib/}" ;;
    /lib/*) printf '/usr/lib/%s\n' "${1#/lib/}" ;;
  esac
}

# Installs the copyright file of the Debian package that owns LIB as
# $ROOT/licenses/<package>.copyright. dpkg-query is asked, in order, for the path ldd
# reports, its resolved target, and each of those with the /lib and /usr/lib alias swapped;
# the first answer names the package. Fails when no candidate is known to dpkg.
install_licence() {
  local lib=$1 real candidate owners line package copyright
  real="$(readlink -f -- "$lib")"
  owners=""
  for candidate in "$lib" "$real" "$(usr_lib_alias "$lib")" "$(usr_lib_alias "$real")"; do
    [[ -n "$candidate" ]] || continue
    # A diverted path answers with diversion lines only, and those name no owner.
    if owners="$(dpkg-query -S -- "$candidate" 2>/dev/null | grep -v '^diversion by ')"; then
      break
    fi
  done
  [[ -n "$owners" ]] || fail "no Debian package owns $lib"
  line="${owners%%$'\n'*}"
  package="${line%%:*}"
  [[ "$package" =~ ^[a-z0-9][a-z0-9.+-]+$ ]] || fail "dpkg-query named no package for $lib: $line"
  copyright="/usr/share/doc/$package/copyright"
  [[ -f "$copyright" ]] || fail "the copyright file of $package is missing: $copyright"
  install -o root -g root -m 0644 "$copyright" "$ROOT/licenses/$package.copyright"
}

# The directory of saved copies and of the saved routing value. Only root may read it.
ensure_saved_dir() {
  install -d -o root -g root -m 0700 "$SAVED_DIR"
}

# Replaces PATH with a root-owned text file of MODE that holds TEXT and a newline.
write_root_text() {
  local mode=$1 path=$2 text=$3
  install -o root -g root -m "$mode" /dev/null "$path"
  printf '%s\n' "$text" > "$path"
}

# Writes MANIFEST in the format of decision 0056 rule 6: one line per regular file under the
# tool root, `<sha256> <length> <relative path>`, sorted by bytes, and no line for the
# manifest itself. An entry that is neither a directory nor a regular file would make the
# provider refuse the root, so it fails here instead of writing a manifest for it.
write_manifest() {
  local manifest path digest length
  ensure_saved_dir
  manifest="$(mktemp -- "$SAVED_DIR/manifest.XXXXXX")"
  (
    cd -- "$ROOT" || fail "cannot enter $ROOT"
    [[ -z "$(find . ! -type d ! -type f -print -quit)" ]] \
      || fail "the tool root has an entry that is neither a directory nor a regular file"
    find . -type f ! -path ./MANIFEST -printf '%P\n' | LC_ALL=C sort |
      while IFS= read -r path; do
        digest="$(sha256sum -- "$path")"
        length="$(stat -c %s -- "$path")"
        printf '%s %s %s\n' "${digest%% *}" "$length" "$path"
      done > "$manifest"
  )
  install -o root -g root -m 0644 "$manifest" "$ROOT/MANIFEST"
  rm -- "$manifest"
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
  # Compared as text: a proc file reports size 0, and cmp calls two regular files of
  # different sizes different without reading them.
  [[ "$(cat -- "$CORE_PATTERN")" == "$(cat -- "$SAVED_ROUTING")" ]] \
    || fail "kernel.core_pattern does not match the saved value"
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

main() {
  local verb=${1:-}
  case "$verb" in
    install) (( $# == 3 )) || usage ;;
    save-routing | routing-file | routing-pipe | profile-on | profile-off | \
      launcher-away | launcher-back | fixture-away | fixture-back | \
      nomem-slice-on | nomem-slice-off | tool-modify | tool-restore | \
      tool-extra-on | tool-extra-off | tool-missing-on | tool-missing-off | \
      manifest-away | manifest-back | payload-break | payload-restore) (( $# == 1 )) || usage ;;
    *) usage ;;
  esac
  if (( EUID != 0 )); then
    fail "run as root through sudo"
  fi
  case "$verb" in
    install) install_tool_root "$2" "$3" ;;
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
    tool-modify) tool_modify ;;
    tool-restore) tool_restore ;;
    tool-extra-on) tool_extra_on ;;
    tool-extra-off) tool_extra_off ;;
    tool-missing-on) tool_missing_on ;;
    tool-missing-off) tool_missing_off ;;
    manifest-away) move_to_saved "$ROOT/MANIFEST" "$SAVED_DIR/MANIFEST" ;;
    manifest-back) move_from_saved "$ROOT/MANIFEST" "$SAVED_DIR/MANIFEST" ;;
    payload-break) payload_break ;;
    payload-restore) payload_restore ;;
  esac
}

main "$@"
