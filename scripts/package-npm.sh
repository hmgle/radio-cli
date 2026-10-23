#!/bin/bash
# Assemble the npm packages for one release target, or only the wrapper package.
#
# Usage:
#   scripts/package-npm.sh <target>    # platform package plus the wrapper package
#   scripts/package-npm.sh --wrapper   # wrapper package only
#
# The platform package needs the release binary that scripts/package-release.sh
# leaves at target/dist/radio-cli-v<version>-<target>/radio. Every package is
# stamped with the version from Cargo.toml so the four npm packages and the
# crate always agree.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -1)
test -n "$version" || {
    printf '%s\n' 'Cannot read the crate version from Cargo.toml.' >&2
    exit 1
}

target=${1:-}
case "$target" in
    --wrapper)
        platform=''
        ;;
    x86_64-unknown-linux-musl)
        platform=linux-x64
        package_os=linux
        package_cpu=x64
        ;;
    aarch64-apple-darwin)
        platform=darwin-arm64
        package_os=darwin
        package_cpu=arm64
        ;;
    x86_64-apple-darwin)
        platform=darwin-x64
        package_os=darwin
        package_cpu=x64
        ;;
    '')
        printf 'Usage: %s <target> | --wrapper\n' "$0" >&2
        exit 2
        ;;
    *)
        printf 'Unsupported target: %s\n' "$target" >&2
        exit 1
        ;;
esac

out=npm/dist
wrapper_dir="$out/@hmgle/radio-cli"
package_dir="$out/@hmgle/radio-cli-$platform"

rm -rf -- "$wrapper_dir"
test -z "$platform" || rm -rf -- "$package_dir"

# The wrapper package carries only the launcher; the binary lives in the
# platform packages listed as its optional dependencies.
mkdir -p "$wrapper_dir/bin"
sed -e "s/__VERSION__/$version/g" npm/radio-cli/package.json >"$wrapper_dir/package.json"
cp npm/radio-cli/bin/radio.js "$wrapper_dir/bin/radio.js"
chmod 755 "$wrapper_dir/bin/radio.js"
cp npm/radio-cli/README.md "$wrapper_dir/README.md"

if test -n "$platform"; then
    binary="target/dist/radio-cli-v$version-$target/radio"
    test -x "$binary" || {
        printf 'Missing %s; run scripts/package-release.sh %s first.\n' "$binary" "$target" >&2
        exit 1
    }
    mkdir -p "$package_dir/bin"
    sed -e "s/__VERSION__/$version/g" \
        -e "s/__PLATFORM__/$platform/g" \
        -e "s/__OS__/$package_os/g" \
        -e "s/__CPU__/$package_cpu/g" \
        npm/platform/package.json >"$package_dir/package.json"
    sed -e "s/__PLATFORM__/$platform/g" npm/platform/README.md >"$package_dir/README.md"
    cp "$binary" "$package_dir/bin/radio"
    chmod 755 "$package_dir/bin/radio"
fi

printf 'Created %s\n' "$wrapper_dir"
test -z "$platform" || printf 'Created %s\n' "$package_dir"
