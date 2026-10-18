#!/bin/sh
set -eu

# Run from the repository root on Linux x86_64.
release_target=x86_64-unknown-linux-musl
cargo build --release --locked --target "$release_target"
release_binary="target/$release_target/release/radio"
release_version=$("$release_binary" --version | awk '{print $2}')
release_name="radio-cli-v$release_version-$release_target"
release_dir="target/dist/$release_name"
mkdir -p "$release_dir/completions" "$release_dir/man"
cp "$release_binary" "$release_dir/radio"
git archive HEAD README.md README.zh-CN.md LICENSE docs | tar -x -C "$release_dir"
for release_shell in bash zsh fish; do
    "$release_binary" completions "$release_shell" > "$release_dir/completions/radio.$release_shell"
done
mv "$release_dir/completions/radio.zsh" "$release_dir/completions/_radio"
"$release_binary" man > "$release_dir/man/radio.1"
{
    git rev-parse HEAD
    git rev-parse 'HEAD^{tree}'
    rustc --version
    printf '%s\n' "cargo build --release --locked --target $release_target"
    sha256sum Cargo.lock
} > "$release_dir/BUILDINFO"
tar -czf "target/dist/$release_name.tar.gz" -C target/dist "$release_name"
(cd target/dist && sha256sum "$release_name.tar.gz" > SHA256SUMS)
printf '%s\n' "target/dist/$release_name.tar.gz"
