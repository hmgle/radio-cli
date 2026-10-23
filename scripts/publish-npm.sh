#!/bin/bash
# Publish one assembled package, or skip its immutable version on a rerun.
set -euo pipefail

if (( $# != 1 )); then
    printf 'Usage: %s <package-directory>\n' "$0" >&2
    exit 2
fi
package_dir=$1
package_spec=$(node -e '
    const fs = require("node:fs");
    const pkg = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    process.stdout.write(pkg.name + "@" + pkg.version);
' "$package_dir/package.json")
lookup_dir=$(mktemp -d)
trap 'rm -rf -- "$lookup_dir"' EXIT

if npm view "$package_spec" version --json --prefer-online >"$lookup_dir/result" 2>"$lookup_dir/error"; then
    # Fail closed on an empty or unexpected registry response.
    node -e '
        const fs = require("node:fs");
        const pkg = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
        try {
            const value = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
            const versions = Array.isArray(value) ? value : [value];
            if (versions.length !== 1 || versions[0] !== pkg.version) {
                throw new Error("expected exactly one matching version");
            }
        } catch (error) {
            const spec = pkg.name + "@" + pkg.version;
            process.stderr.write("Unexpected registry response for " + spec +
                ": " + error.message + "\n");
            process.exit(1);
        }
    ' "$package_dir/package.json" "$lookup_dir/result"
    printf 'Already published %s; skipping.\n' "$package_spec"
else
    # Only a missing package/version permits an upload. Authentication,
    # connectivity and registry errors must remain failures.
    if ! node -e '
        const fs = require("node:fs");
        const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
        if (result.error?.code !== "E404") process.exit(1);
    ' "$lookup_dir/result"; then
        cat "$lookup_dir/result" "$lookup_dir/error" >&2
        exit 1
    fi
    npm publish "$package_dir" --provenance
fi
