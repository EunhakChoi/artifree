# Sourced by prepare.sh and run.sh: the workspace's package version and its tool root (decision
# 0058 rule 19), derived in one place so both scripts name the root the provider's TOOL_ROOT
# names. It also loads the operator's packaging functions (scripts/tools/package-root.sh).

VERSION_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly VERSION_DIR
# shellcheck source=../tools/package-root.sh
source "$VERSION_DIR/../tools/package-root.sh"

# The repository's package version, from the workspace manifest. The job's server binary must
# name the same version, and the provider's TOOL_ROOT is the root of that version.
repo_version() {
  awk '/^\[workspace\.package\]/ { section = 1; next } /^\[/ { section = 0 }
    section && $1 == "version" { gsub(/"/, "", $3); print $3; exit }' "$VERSION_DIR/../../Cargo.toml"
}
VERSION="$(repo_version)"
readonly VERSION
is_version "$VERSION" || package_fail "the workspace names no package version: $VERSION"
ROOT="$(root_of_version "$VERSION")"
readonly ROOT
