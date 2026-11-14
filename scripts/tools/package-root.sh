#!/usr/bin/env bash
# The operator's packaging script for an execution tool root (decision 0058 rule 19).
#
# Usage: package-root.sh SERVER [LOCK]
#
# Run as root, once for each server release, on the supported host. It assembles
# /opt/artifree/tools/<version>, where <version> is the package version that `SERVER --version`
# names, and loads the AppArmor launcher profile of that root. The root holds the pinned
# launcher and its licence, SERVER as the bootstrap with its libraries and their licences, the
# bundler and the packages of the lock list LOCK (by default react-v1.lock beside this script)
# with their licences, and the manifest of every file, all owned by root without group or other write
# bits, as the provider requires. The script refuses a root that exists: a root is never
# replaced in place (decision 0042 rule 3). A failed run removes the root it made, so it can be
# run again. Nothing is downloaded but the pinned launcher package and the archives of the lock
# list. Every archive is downloaded and checked against its SHA-256 before the root is made, so a
# wrong archive leaves no root behind. The script refuses to downgrade a newer Bubblewrap that
# the host already has.
#
# scripts/enforcement/prepare.sh sources this file, so the enforcement job builds its root with
# the same functions. The job adds its test programs before the manifest, and its verbs that
# rewrite the root in place belong to the test harness, not to this script.
set -euo pipefail

# The parent of every root. Its ancestors must be root-owned and not writable by others.
readonly TOOLS_DIR=/opt/artifree/tools
# The one Bubblewrap revision the launcher is copied from.
readonly BUBBLEWRAP_VERSION=0.9.0-1ubuntu0.3
readonly APPARMOR_DIR=/etc/apparmor.d
# The lock list beside this script.
readonly DEFAULT_LOCK="$(dirname -- "${BASH_SOURCE[0]}")/react-v1.lock"

package_fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

# Prints the package version that SERVER names in `--version`, for example 0.1.0.
server_version() {
  local server=$1 line version
  line="$("$server" --version)" || package_fail "$server --version failed"
  version="${line#artifree-server }"
  [[ "$line" == "artifree-server $version" ]] || package_fail "$server --version printed $line"
  is_version "$version" || package_fail "$server names no package version: $line"
  printf '%s\n' "$version"
}

# Whether VERSION is a package version that may name a root: digits, dots, and a pre-release or
# build suffix of letters, digits, dots and hyphens, with no path separator.
is_version() {
  [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]
}

# The root of VERSION.
root_of_version() {
  printf '%s/%s\n' "$TOOLS_DIR" "$1"
}

# The AppArmor profile name of VERSION's launcher. It carries the version, so the roots of two
# versions keep their profiles side by side during an upgrade.
launcher_profile_name() {
  printf 'artifree-launcher-%s\n' "$1"
}

# Makes ROOT's parent directories root-owned 0755 and ROOT itself, refusing an existing root.
# ROOT is made by one mkdir, which fails when anything is at that path, so two runs never share
# a root.
begin_root() {
  install -d -o root -g root -m 0755 /opt/artifree "$TOOLS_DIR"
  mkdir -m 0755 -- "$ROOT" 2> /dev/null || package_fail "refusing to write over the existing root $ROOT"
  install -d -o root -g root -m 0755 "$ROOT/bin" "$ROOT/lib" "$ROOT/lib64" "$ROOT/licenses" "$ROOT/etc"
}

# Installs the pinned Bubblewrap package and copies its launcher into ROOT with its licence. A
# copy, not a link: the AppArmor profile names the launcher by its path in the root.
install_launcher() {
  local installed
  installed="$(dpkg-query -W -f='${Version}' bubblewrap 2> /dev/null || true)"
  if [[ -n "$installed" ]] && dpkg --compare-versions "$installed" gt "$BUBBLEWRAP_VERSION"; then
    package_fail "bubblewrap $installed is newer than the pinned $BUBBLEWRAP_VERSION; this script does not downgrade it"
  fi
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  apt-get install -y --no-install-recommends "bubblewrap=$BUBBLEWRAP_VERSION"
  install -o root -g root -m 0755 /usr/bin/bwrap "$ROOT/bin/bwrap"
  install_licence /usr/bin/bwrap
}

# Installs SERVER, stripped, as the bootstrap (decision 0057 rule 4), with its libraries. It is
# stripped because every admission hashes every file of the root.
install_server() {
  local server=$1
  [[ -f "$server" && -x "$server" ]] || package_fail "the server binary is not an executable file: $server"
  install -s -o root -g root -m 0755 "$server" "$ROOT/bin/artifree-server"
  copy_libraries "$server"
}

# Copies the shared libraries BINARY needs to the paths the sandbox gives them, and installs
# the licence of the Debian package that owns each copied library. The sandbox binds the root's
# lib and lib64 as /lib and /lib64 and nothing else, so a library that ldd reports under
# /usr/lib (usrmerged hosts such as Ubuntu 24.04) is copied to the path it has inside.
copy_libraries() {
  local binary=$1 ldd_output lib dest
  ldd_output="$(ldd "$binary")"
  case "$ldd_output" in
    *"not found"*) package_fail "$binary has a library that does not resolve" ;;
  esac
  while IFS= read -r lib; do
    case "$lib" in
      /lib/* | /lib64/*) dest="$ROOT$lib" ;;
      /usr/lib/*) dest="$ROOT/lib/${lib#/usr/lib/}" ;;
      /usr/lib64/*) dest="$ROOT/lib64/${lib#/usr/lib64/}" ;;
      *) package_fail "$binary needs a library outside /lib, /lib64, /usr/lib and /usr/lib64: $lib" ;;
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

# Installs the copyright file of the Debian package that owns FILE as
# ROOT/licenses/<package>.copyright. dpkg-query is asked, in order, for the path given, its
# resolved target, and each of those with the /lib and /usr/lib alias swapped; the first answer
# names the package. Fails when no candidate is known to dpkg.
install_licence() {
  local file=$1 real candidate owners line package copyright
  real="$(readlink -f -- "$file")"
  owners=""
  for candidate in "$file" "$real" "$(usr_lib_alias "$file")" "$(usr_lib_alias "$real")"; do
    [[ -n "$candidate" ]] || continue
    # A diverted path answers with diversion lines only, and those name no owner.
    if owners="$(dpkg-query -S -- "$candidate" 2> /dev/null | grep -v '^diversion by ')"; then
      break
    fi
  done
  [[ -n "$owners" ]] || package_fail "no Debian package owns $file"
  line="${owners%%$'\n'*}"
  package="${line%%:*}"
  [[ "$package" =~ ^[a-z0-9][a-z0-9.+-]+$ ]] || package_fail "dpkg-query named no package for $file: $line"
  copyright="/usr/share/doc/$package/copyright"
  [[ -f "$copyright" ]] || package_fail "the copyright file of $package is missing: $copyright"
  install -o root -g root -m 0644 "$copyright" "$ROOT/licenses/$package.copyright"
}

# Replaces PATH with a root-owned text file of MODE that holds TEXT and a newline.
write_root_text() {
  local mode=$1 path=$2 text=$3
  install -o root -g root -m "$mode" /dev/null "$path"
  printf '%s\n' "$text" > "$path"
}

# Fails, naming each offender, unless ROOT, everything under it and each of its ancestors are
# root's, with no group or other write bit (an ancestor may have them with its sticky bit set)
# and no set-ID bit: the check the provider makes at every admission (decision 0058 rule 19).
check_root_protected() {
  local offenders dir uid mode
  offenders="$(find "$ROOT" \( ! -uid 0 -o -perm /6022 \) -printf '%m %U %p\n')"
  [[ -z "$offenders" ]] || package_fail "the root has entries others could change:"$'\n'"$offenders"
  dir="$(dirname -- "$ROOT")"
  while :; do
    read -r uid mode < <(stat -c '%u %a' -- "$dir")
    mode=$((8#$mode))
    if ((uid != 0 || (mode & 06000) != 0 || ((mode & 022) != 0 && (mode & 01000) == 0))); then
      package_fail "the ancestor $dir could be changed by someone other than root: uid $uid, mode $(printf '%o' "$mode")"
    fi
    [[ "$dir" != / ]] || break
    dir="$(dirname -- "$dir")"
  done
}

# Writes ROOT/MANIFEST in the format of decision 0056 rule 6: one line per regular file under
# the root, `<sha256> <length> <relative path>`, sorted by bytes, and no line for the manifest
# itself. It first checks that the root is protected. An entry that is neither a directory nor a regular file would make the provider
# refuse the root, so it fails here instead of writing a manifest for it.
write_manifest() {
  local manifest
  check_root_protected
  manifest="$(mktemp -- "$TOOLS_DIR/.manifest.XXXXXX")"
  (
    cd -- "$ROOT" || package_fail "cannot enter $ROOT"
    [[ -z "$(find . ! -type d ! -type f -print -quit)" ]] \
      || package_fail "the root has an entry that is neither a directory nor a regular file"
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

# Prints the archive lines of LOCK, without comments and blank lines, after checking each:
# KIND NAME VERSION SHA-256 URL LICENCE, with a known kind, a lowercase package name, a package
# version, a lowercase SHA-256 and an https URL of the npm registry.
lock_entries() {
  local lock=$1 kind name version digest url licence extra
  [[ -f "$lock" ]] || package_fail "the lock list is not a file: $lock"
  while read -r kind name version digest url licence extra; do
    [[ -n "$kind" && "$kind" != \#* ]] || continue
    [[ "$kind" =~ ^(bundler|licence|package)$ ]] || package_fail "unknown kind in the lock list: $kind"
    [[ "$name" =~ ^[a-z0-9][a-z0-9.-]*$ ]] || package_fail "bad name in the lock list: $name"
    is_version "$version" || package_fail "bad version of $name in the lock list: $version"
    [[ "$digest" =~ ^[0-9a-f]{64}$ ]] || package_fail "bad SHA-256 of $name in the lock list"
    [[ "$url" =~ ^https://registry\.npmjs\.org/[^[:space:]]+\.tgz$ ]] \
      || package_fail "bad URL of $name in the lock list: $url"
    [[ -n "$licence" && -z "$extra" ]] || package_fail "bad licence field of $name in the lock list"
    printf '%s %s %s %s %s %s\n' "$kind" "$name" "$version" "$digest" "$url" "$licence"
  done < "$lock"
}

# Downloads every archive of LOCK into DIR as NAME.tgz and checks its SHA-256. A mismatch fails
# before any archive is unpacked, and before the root exists.
fetch_locked() {
  local lock=$1 dir=$2 kind name version digest url licence actual entries
  # Read whole first: a failure of lock_entries ends the script here, not as an early end of list.
  entries="$(lock_entries "$lock")"
  [[ -n "$entries" ]] || package_fail "the lock list names no archive: $lock"
  while read -r kind name version digest url licence; do
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
      --output "$dir/$name.tgz" -- "$url" || package_fail "the download of $name $version failed"
    actual="$(sha256sum -- "$dir/$name.tgz")"
    actual="${actual%% *}"
    [[ "$actual" == "$digest" ]] \
      || package_fail "the SHA-256 of $name $version is $actual, the lock list names $digest"
  done <<< "$entries"
}

# Unpacks ARCHIVE into the new directory DEST after checking that it holds only directories and
# regular files under package/, with no absolute or parent path. Owners and modes in the archive
# are ignored.
unpack_archive() {
  local archive=$1 dest=$2 listing
  listing="$(tar --list --verbose --gzip --file "$archive")" || package_fail "cannot read $archive"
  while IFS= read -r line; do
    case "$line" in
      d* | -*) ;;
      *) package_fail "$archive holds an entry that is neither a directory nor a regular file" ;;
    esac
  done <<< "$listing"
  while IFS= read -r path; do
    case "$path" in
      package/* | package) ;;
      *) package_fail "$archive holds a path outside package/: $path" ;;
    esac
    case "/$path/" in
      */../* | */./*) package_fail "$archive holds a path with a dot segment: $path" ;;
    esac
  done < <(tar --list --gzip --file "$archive")
  mkdir -m 0700 -- "$dest"
  tar --extract --gzip --no-same-owner --no-same-permissions --file "$archive" --directory "$dest"
}

# Installs every regular file under SOURCE into TARGET with MODE, root-owned, keeping its path.
install_tree() {
  local source=$1 target=$2 mode=$3 path
  while IFS= read -r -d '' path; do
    install -D -o root -g root -m "$mode" -- "$source/$path" "$target/$path"
  done < <(cd -- "$source" && find . -type f -printf '%P\0')
  find "$target" -type d -exec chmod 0755 {} +
}

# Installs the archives that fetch_locked put in DIR into ROOT, as the lock list's kinds say.
install_locked() {
  local lock=$1 dir=$2 kind name version digest url licence unpacked entries
  entries="$(lock_entries "$lock")"
  while read -r kind name version digest url licence; do
    unpacked="$dir/$name"
    unpack_archive "$dir/$name.tgz" "$unpacked"
    case "$kind" in
      bundler)
        [[ -f "$unpacked/package/bin/esbuild" ]] || package_fail "$name has no package/bin/esbuild"
        install -o root -g root -m 0755 -- "$unpacked/package/bin/esbuild" "$ROOT/bin/esbuild"
        ;;
      licence)
        [[ -f "$unpacked/package/LICENSE.md" ]] || package_fail "$name has no package/LICENSE.md"
        install -o root -g root -m 0644 -- "$unpacked/package/LICENSE.md" \
          "$ROOT/licenses/$name.LICENSE.md"
        ;;
      package)
        [[ -f "$unpacked/package/LICENSE" ]] || package_fail "$name has no package/LICENSE"
        install -d -o root -g root -m 0755 "$ROOT/modules"
        install_tree "$unpacked/package" "$ROOT/modules/$name" 0644
        install -o root -g root -m 0644 -- "$unpacked/package/LICENSE" "$ROOT/licenses/$name.LICENSE"
        ;;
    esac
  done <<< "$entries"
  [[ -f "$ROOT/bin/esbuild" ]] || package_fail "the lock list installed no bundler"
}

# The profile file of VERSION's launcher.
launcher_profile_file() {
  printf '%s/%s\n' "$APPARMOR_DIR" "$(launcher_profile_name "$1")"
}

# Writes and loads the AppArmor profile that lets VERSION's launcher, by its path in ROOT,
# create user namespaces while the system-wide restriction stays on.
load_launcher_profile() {
  local version=$1 file
  file="$(launcher_profile_file "$version")"
  cat > "$file" << EOF
abi <abi/4.0>,
include <tunables/global>
profile $(launcher_profile_name "$version") $ROOT/bin/bwrap flags=(unconfined) {
  userns,
}
EOF
  apparmor_parser -r "$file"
}

# Unloads and removes VERSION's launcher profile.
unload_launcher_profile() {
  local file
  file="$(launcher_profile_file "$1")"
  apparmor_parser -R "$file"
  rm -- "$file"
}

# Removes the root this run made and its launcher profile file, after a failed run, so that a
# retry starts from nothing. begin_root refused any root that was there before.
abandon_root() {
  local version=$1
  rm -rf -- "$ROOT"
  rm -f -- "$(launcher_profile_file "$version")"
}

package_main() {
  (($# == 1 || $# == 2)) || {
    printf 'usage: package-root.sh SERVER [LOCK]\n' >&2
    exit 2
  }
  ((EUID == 0)) || package_fail "run as root"
  local server=$1 lock=${2:-$DEFAULT_LOCK} version
  version="$(server_version "$server")"
  ROOT="$(root_of_version "$version")"
  readonly ROOT
  # A cheap early refusal, so that a second run downloads nothing; begin_root's mkdir is the
  # check that holds.
  [[ ! -e "$ROOT" && ! -L "$ROOT" ]] || package_fail "refusing to write over the existing root $ROOT"
  ARCHIVES="$(mktemp -d)"
  readonly ARCHIVES
  # shellcheck disable=SC2064 # The directory is fixed now, so it is expanded now.
  trap "rm -rf -- '$ARCHIVES'" EXIT
  fetch_locked "$lock" "$ARCHIVES"
  begin_root
  # shellcheck disable=SC2064 # The version is fixed now, so it is expanded now.
  trap "abandon_root '$version'; rm -rf -- '$ARCHIVES'" EXIT
  install_launcher
  install_server "$server"
  install_locked "$lock" "$ARCHIVES"
  write_manifest
  load_launcher_profile "$version"
  trap - EXIT
  rm -rf -- "$ARCHIVES"
  printf '%s\n' "$ROOT"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  package_main "$@"
fi
