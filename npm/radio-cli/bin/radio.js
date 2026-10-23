#!/usr/bin/env node
'use strict';

// Launcher for @hmgle/radio-cli. The prebuilt binary ships in a platform
// package that is installed as an optional dependency, so resolve that package
// here and hand the command line over to the binary.
const { spawnSync } = require('node:child_process');
const { createRequire } = require('node:module');

const PLATFORM_PACKAGES = {
  'darwin-arm64': '@hmgle/radio-cli-darwin-arm64',
  'darwin-x64': '@hmgle/radio-cli-darwin-x64',
  'linux-x64': '@hmgle/radio-cli-linux-x64',
};

const platform = `${process.platform}-${process.arch}`;
const pkg = PLATFORM_PACKAGES[platform];
if (!pkg) {
  process.stderr.write(
    `radio: no prebuilt package for ${platform}; supported platforms are ` +
      `${Object.keys(PLATFORM_PACKAGES).join(', ')}\n`,
  );
  process.exit(1);
}

let binary;
try {
  binary = createRequire(__filename).resolve(`${pkg}/bin/radio`);
} catch {
  process.stderr.write(
    `radio: ${pkg} is missing. Reinstall with ` +
      `"npm install -g @hmgle/radio-cli"; installing with --omit=optional or ` +
      `--ignore-scripts skips the platform package.\n`,
  );
  process.exit(1);
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: 'inherit' });
if (result.error) {
  process.stderr.write(`radio: cannot run ${binary}: ${result.error.message}\n`);
  process.exit(1);
}
// A signal leaves the status null; report failure instead of success.
process.exit(result.status === null ? 1 : result.status);
