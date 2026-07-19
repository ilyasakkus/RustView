#!/usr/bin/env bash

set -euo pipefail

if [[ $# -ne 1 || ! "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Usage: $0 <major.minor.patch>" >&2
    exit 2
fi

version="$1"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/.." && pwd)"
target_root="${CARGO_TARGET_DIR:-$repo_root/target}"
dist_dir="$repo_root/dist"
package_name="RustView-macOS-universal"
archive="$dist_dir/$package_name.zip"
stage_root="$(mktemp -d "${TMPDIR:-/tmp}/rustview-macos.XXXXXX")"

cleanup() {
    rm -rf -- "$stage_root"
}
trap cleanup EXIT

require_file() {
    if [[ ! -f "$1" ]]; then
        echo "Required build output is missing: $1" >&2
        exit 1
    fi
}

arm_dir="$target_root/aarch64-apple-darwin/release"
intel_dir="$target_root/x86_64-apple-darwin/release"
require_file "$arm_dir/rustview-desktop"
require_file "$arm_dir/rustview-relay"
require_file "$intel_dir/rustview-desktop"
require_file "$intel_dir/rustview-relay"

package_root="$stage_root/$package_name"
app_contents="$package_root/RustView.app/Contents"
mkdir -p "$app_contents/MacOS" "$dist_dir"

lipo -create \
    "$arm_dir/rustview-desktop" \
    "$intel_dir/rustview-desktop" \
    -output "$app_contents/MacOS/RustView"
lipo -create \
    "$arm_dir/rustview-relay" \
    "$intel_dir/rustview-relay" \
    -output "$package_root/rustview-relay"
lipo -verify_arch arm64 x86_64 "$app_contents/MacOS/RustView"
lipo -verify_arch arm64 x86_64 "$package_root/rustview-relay"
chmod 755 "$app_contents/MacOS/RustView" "$package_root/rustview-relay"

cp "$repo_root/packaging/macos/Info.plist" "$app_contents/Info.plist"
/usr/libexec/PlistBuddy \
    -c "Set :CFBundleShortVersionString $version" \
    -c "Set :CFBundleVersion $version" \
    "$app_contents/Info.plist"
plutil -lint "$app_contents/Info.plist"

cp "$repo_root/README.md" "$repo_root/LICENSE-MIT" "$package_root/"
codesign --force --deep --sign - "$package_root/RustView.app"
codesign --force --sign - "$package_root/rustview-relay"

if [[ -e "$archive" ]]; then
    rm -- "$archive"
fi
ditto -c -k --sequesterRsrc --keepParent "$package_root" "$archive"
unzip -tq "$archive"
echo "Created $archive"
