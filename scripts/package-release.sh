#!/bin/bash
set -euo pipefail

# Run from a clean tracked checkout on Linux x86_64.
git diff --quiet HEAD -- || { printf '%s\n' 'Commit tracked changes before packaging.' >&2; exit 1; }
release_target=x86_64-unknown-linux-musl
cargo build --release --locked --target "$release_target"
release_binary="target/$release_target/release/radio"
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
    "$release_binary" completions "$release_shell" > "$release_dir/completions/radio.$release_shell"
done
mv "$release_dir/completions/radio.zsh" "$release_dir/completions/_radio"
"$release_binary" --lang en man > "$release_dir/man/radio.1"
{
    git rev-parse HEAD
    git rev-parse 'HEAD^{tree}'
    rustc --version
    printf '%s\n' "cargo build --release --locked --target $release_target"
    sha256sum Cargo.lock
} > "$release_dir/BUILDINFO"
while IFS= read -r -d '' release_file; do
    test -s "$release_file" || { printf 'Empty release file: %s\n' "$release_file" >&2; exit 1; }
done < <(find "$release_dir" -type f -print0)
release_epoch=$(git show -s --format=%ct HEAD)
tar --sort=name --mtime="@$release_epoch" --owner=0 --group=0 --numeric-owner \
    -cf - -C "$release_staging" "$release_name" | gzip -n > "target/dist/$release_name.tar.gz"
(cd target/dist && sha256sum "$release_name.tar.gz" > SHA256SUMS)
# Only this task-owned, version-validated output directory is replaced.
rm -rf -- "target/dist/$release_name"
mv "$release_dir" "target/dist/$release_name"
printf '%s\n' "target/dist/$release_name.tar.gz"
