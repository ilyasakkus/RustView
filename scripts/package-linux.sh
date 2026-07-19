#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/.." && pwd)"
target_root="${CARGO_TARGET_DIR:-$repo_root/target}"
release_dir="$target_root/release"
dist_dir="$repo_root/dist"
package_name="RustView-Linux-x86_64"
archive="$dist_dir/$package_name.tar.gz"
stage_root="$(mktemp -d "${TMPDIR:-/tmp}/rustview-linux.XXXXXX")"

cleanup() {
    rm -rf -- "$stage_root"
}
trap cleanup EXIT

for binary in rustview-desktop rustview-relay; do
    path="$release_dir/$binary"
    if [[ ! -f "$path" ]]; then
        echo "Required build output is missing: $path" >&2
        exit 1
    fi
    if ! file "$path" | grep -qE 'ELF 64-bit.*(x86-64|x86_64)'; then
        echo "Expected an x86_64 Linux executable: $path" >&2
        file "$path" >&2
        exit 1
    fi
done

package_root="$stage_root/$package_name"
mkdir -p "$package_root" "$dist_dir"
install -m 755 "$release_dir/rustview-desktop" "$package_root/rustview"
install -m 755 "$release_dir/rustview-relay" "$package_root/rustview-relay"
install -m 644 "$repo_root/README.md" "$repo_root/LICENSE-MIT" "$package_root/"

if [[ -e "$archive" ]]; then
    rm -- "$archive"
fi
tar -C "$stage_root" -czf "$archive" "$package_name"
tar -tzf "$archive" >/dev/null
echo "Created $archive"
