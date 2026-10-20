#!/bin/bash
set -euo pipefail

# Build and smoke-test on the target's native host from a clean tracked checkout.
git diff --quiet HEAD -- || { printf '%s\n' 'Commit tracked changes before packaging.' >&2; exit 1; }
case "$(uname -s):$(uname -m)" in
    Linux:x86_64) release_native=x86_64-unknown-linux-musl ;;
    Darwin:arm64) release_native=aarch64-apple-darwin ;;
    Darwin:x86_64) release_native=x86_64-apple-darwin ;;
    *) printf '%s\n' 'Unsupported release host.' >&2; exit 1 ;;
esac
if (( $# > 1 )); then
    printf 'Usage: %s [target]\n' "$0" >&2
    exit 1
fi
release_target=${1:-$release_native}
if [[ "$release_target" != "$release_native" ]]; then
    printf 'Build and validate %s on its native host (this host: %s).\n' "$release_target" "$release_native" >&2
    exit 1
fi
release_tar=tar
if [[ "$release_target" == *-apple-darwin ]]; then
    release_tar=gtar
fi
command -v "$release_tar" >/dev/null || { printf '%s\n' 'GNU tar is required (brew install gnu-tar on macOS).' >&2; exit 1; }
release_sha256() {
    if command -v sha256sum >/dev/null; then
        sha256sum "$@"
    else
        shasum -a 256 "$@"
    fi
}
cargo build --release --locked --target "$release_target"
release_binary="target/$release_target/release/radio"
release_format=$(file "$release_binary")
printf '%s\n' "$release_format"
case "$release_target" in
    x86_64-unknown-linux-musl)
        grep -Eq 'ELF 64-bit.*x86-64.*static(ally linked|-pie linked)' <<< "$release_format" ;;
    aarch64-apple-darwin)
        test "$(lipo -archs "$release_binary")" = arm64 ;;
    x86_64-apple-darwin)
        test "$(lipo -archs "$release_binary")" = x86_64 ;;
esac
release_version=$("$release_binary" --version | awk '{print $2}')
case "$release_version" in
    ''|*[!0-9A-Za-z.+-]*) printf '%s\n' 'Invalid release version.' >&2; exit 1 ;;
esac
release_name="radio-cli-v$release_version-$release_target"
mkdir -p target/dist
release_staging=$(mktemp -d target/dist/.package.XXXXXX)
trap 'rm -rf -- "$release_staging"' EXIT
release_dir="$release_staging/$release_name"
mkdir -p "$release_dir/completions" "$release_dir/man"
cp "$release_binary" "$release_dir/radio"
git archive HEAD README.md README.zh-CN.md LICENSE CHANGELOG.md config.example.toml \
    docs/tui.md docs/json-contract.md docs/validation.md | tar -x -C "$release_dir"
for release_shell in bash zsh fish; do
    "$release_binary" --lang en completions "$release_shell" > "$release_dir/completions/radio.$release_shell"
done
mv "$release_dir/completions/radio.zsh" "$release_dir/completions/_radio"
"$release_binary" --lang en man > "$release_dir/man/radio.1"
{
    git rev-parse HEAD
    git rev-parse 'HEAD^{tree}'
    rustc --version
    printf 'target: %s\n' "$release_target"
    printf 'host: %s\n' "$(uname -srm)"
    printf '%s\n' "$release_format"
    printf '%s\n' "cargo build --release --locked --target $release_target"
    release_sha256 Cargo.lock
} > "$release_dir/BUILDINFO"
while IFS= read -r -d '' release_file; do
    test -s "$release_file" || { printf 'Empty release file: %s\n' "$release_file" >&2; exit 1; }
done < <(find "$release_dir" -type f -print0)
release_epoch=$(git show -s --format=%ct HEAD)
"$release_tar" --sort=name --mtime="@$release_epoch" --owner=0 --group=0 --numeric-owner \
    -cf - -C "$release_staging" "$release_name" | gzip -n > "target/dist/$release_name.tar.gz"
mkdir "$release_staging/check"
"$release_tar" -xf "target/dist/$release_name.tar.gz" -C "$release_staging/check"
release_extracted="$release_staging/check/$release_name/radio"
test "$("$release_extracted" --version)" = "radio $release_version"
: > "$release_staging/doctor.toml"
"$release_extracted" --lang en --config "$release_staging/doctor.toml" \
    --data-dir "$release_staging/doctor-data" doctor
(cd target/dist && release_sha256 "radio-cli-v$release_version-"*.tar.gz > SHA256SUMS)
# Only this task-owned, version-validated output directory is replaced.
rm -rf -- "target/dist/$release_name"
mv "$release_dir" "target/dist/$release_name"
printf '%s\n' "target/dist/$release_name.tar.gz"
